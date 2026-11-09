//! Word <-> PDF conversion helpers.
//!
//! Word to PDF uses Microsoft Word's COM automation because it produces the
//! highest-fidelity result and Word is already required to edit the source
//! files. PDF to Word builds a DOCX from text extracted by the frontend
//! (pdf.js), preserving page breaks, line breaks and paragraph gaps without
//! depending on a local Office installation.

use std::fs;
use std::io::{Cursor, Read};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use docx_rs::{BreakType, Docx, LineSpacing, Paragraph, Run, RunFonts};
use serde::Deserialize;

/// Word can take a while on large documents, but should never hang forever.
const WORD_TIMEOUT: Duration = Duration::from_secs(180);

const WORD_TO_PDF_SCRIPT: &str = r#"
param(
  [Parameter(Mandatory = $true)][string]$InputPath,
  [Parameter(Mandatory = $true)][string]$OutputPath
)
$ErrorActionPreference = 'Stop'
$word = $null
$ownsInstance = $false
$oldVisible = $true
$oldAlerts = 0
try {
  $before = @(Get-Process WINWORD -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Id)
  $word = New-Object -ComObject Word.Application
  Start-Sleep -Milliseconds 300
  $after = @(Get-Process WINWORD -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Id)
  $newIds = @($after | Where-Object { $before -notcontains $_ })
  # If Word was not running before, we own the instance. If Word was already
  # running, COM may hand us the existing instance; leave it exactly as it was.
  $ownsInstance = ($before.Count -eq 0) -or ($newIds.Count -gt 0)
  try { $oldVisible = $word.Visible } catch {}
  try { $oldAlerts = $word.DisplayAlerts } catch {}
  if ($ownsInstance) { $word.Visible = $false }
  $word.DisplayAlerts = 0
  try { $word.Options.ConfirmConversions = $false } catch {}
  try { $word.Options.DoNotPromptForConvert = $true } catch {}
  $doc = $word.Documents.Open([string]$InputPath, $false, $true, $false)
  try {
    $doc.SaveAs2([string]$OutputPath, [int]17)
  } finally {
    $doc.Close($false)
  }
} finally {
  if ($word -ne $null) {
    if ($ownsInstance) {
      $save = 0
      try { $word.Quit([ref]$save) } catch {}
    } else {
      try { $word.Visible = $oldVisible } catch {}
      try { $word.DisplayAlerts = $oldAlerts } catch {}
    }
    [System.Runtime.InteropServices.Marshal]::ReleaseComObject($word) | Out-Null
  }
}
"#;

#[derive(Debug, Deserialize)]
struct TextPayload {
    #[serde(default)]
    pages: Vec<Vec<String>>,
}

/// Convert a Word document to PDF using Word COM automation.
///
/// The PDF is written to a temporary file next to the requested output and
/// only copied into place after Word reports success, so a failed conversion
/// never truncates an existing file.
pub fn word_to_pdf(input: &str, output: &str) -> Result<(), String> {
    let input_path = Path::new(input);
    if !input_path.is_file() {
        return Err("The Word document could not be found.".into());
    }
    let extension = input_path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(extension.as_str(), "doc" | "docx" | "docm" | "rtf" | "odt") {
        return Err("Choose a Word document (.docx or .doc).".into());
    }

    let output_path = Path::new(output);
    let parent = output_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|err| format!("Could not create the output folder: {err}"))?;

    let temp_path = parent.join(format!("pdfzen-convert-{}.pdf", unique_id()));
    let temp_str = temp_path
        .to_str()
        .ok_or_else(|| "The output path contains characters that cannot be used.".to_string())?;

    let result = run_word_script(
        WORD_TO_PDF_SCRIPT,
        &[("InputPath", input), ("OutputPath", temp_str)],
    );

    if let Err(err) = result {
        let _ = fs::remove_file(&temp_path);
        return Err(err);
    }

    let metadata = fs::metadata(&temp_path).map_err(|_| {
        "Microsoft Word finished but did not create a PDF. Check that the document is not damaged or password-protected.".to_string()
    })?;
    if metadata.len() == 0 {
        let _ = fs::remove_file(&temp_path);
        return Err("Microsoft Word produced an empty PDF. Check the source document and try again.".into());
    }

    if let Err(err) = fs::copy(&temp_path, output_path) {
        let _ = fs::remove_file(&temp_path);
        return Err(format!("Could not save the converted PDF: {err}"));
    }
    let _ = fs::remove_file(&temp_path);
    Ok(())
}

/// Build a DOCX from the text payload extracted by the frontend.
///
/// Payload shape: `{ "pages": [ ["paragraph", "paragraph with\nline breaks"] ] }`.
pub fn text_to_word(payload: &str, output: &str) -> Result<(), String> {
    let parsed: TextPayload =
        serde_json::from_str(payload).map_err(|err| format!("Could not read the extracted text: {err}"))?;

    let output_path = Path::new(output);
    let parent = output_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|err| format!("Could not create the output folder: {err}"))?;

    let mut docx = Docx::new()
        .default_size(22)
        .default_fonts(
            RunFonts::new()
                .ascii("Calibri")
                .hi_ansi("Calibri")
                .east_asia("Calibri"),
        );

    let mut wrote_any = false;
    for (page_index, paragraphs) in parsed.pages.iter().enumerate() {
        let mut page_started = false;
        for paragraph in paragraphs {
            let text = sanitize_text(paragraph);
            if text.trim().is_empty() {
                continue;
            }
            let mut item = Paragraph::new().line_spacing(LineSpacing::new().before(0).after(0));
            if !page_started && page_index > 0 {
                item = item.page_break_before(true);
            }
            let lines: Vec<&str> = text.split('\n').collect();
            for (line_index, line) in lines.iter().enumerate() {
                if line_index > 0 {
                    item = item.add_run(Run::new().add_break(BreakType::TextWrapping));
                }
                if !line.is_empty() {
                    item = item.add_run(Run::new().add_text(*line));
                }
            }
            docx = docx.add_paragraph(item);
            wrote_any = true;
            page_started = true;
        }
        if !page_started && page_index > 0 {
            docx = docx.add_paragraph(Paragraph::new().page_break_before(true));
        }
    }

    if !wrote_any {
        docx = docx.add_paragraph(Paragraph::new().add_run(Run::new().add_text(
            "This PDF did not contain selectable text. Scanned pages need OCR before they can be converted to Word.",
        )));
    }

    let mut cursor = Cursor::new(Vec::new());
    docx.pack(&mut cursor)
        .map_err(|err| format!("Could not build the Word document: {err}"))?;
    let bytes = cursor.into_inner();

    let temp_path = parent.join(format!("pdfzen-convert-{}.docx", unique_id()));
    fs::write(&temp_path, &bytes).map_err(|err| format!("Could not write the Word document: {err}"))?;
    if let Err(err) = fs::copy(&temp_path, output_path) {
        let _ = fs::remove_file(&temp_path);
        return Err(format!("Could not save the Word document: {err}"));
    }
    let _ = fs::remove_file(&temp_path);
    Ok(())
}

fn run_word_script(script: &str, args: &[(&str, &str)]) -> Result<(), String> {
    let script_path = std::env::temp_dir().join(format!("pdfzen-convert-{}.ps1", unique_id()));
    fs::write(&script_path, script).map_err(|err| format!("Could not prepare the converter: {err}"))?;

    let before = word_pids();
    let mut command = Command::new("powershell.exe");
    command.args([
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
    ]);
    command.arg(&script_path);
    for (name, value) in args {
        command.arg(format!("-{name}"));
        command.arg(value);
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            let _ = fs::remove_file(&script_path);
            return Err(format!("Could not start the Word converter: {err}"));
        }
    };

    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let (stdout, stderr) = read_output(&mut child);
                let _ = fs::remove_file(&script_path);
                kill_new_word_processes(&before);
                if status.success() {
                    return Ok(());
                }
                return Err(word_error(&stderr, &stdout));
            }
            Ok(None) => {
                if started.elapsed() > WORD_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = fs::remove_file(&script_path);
                    kill_new_word_processes(&before);
                    return Err(
                        "Microsoft Word took too long to convert this document. Close Word, then try again, or save the PDF from Word with File > Save As."
                            .into(),
                    );
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = fs::remove_file(&script_path);
                kill_new_word_processes(&before);
                return Err(format!("The Word converter stopped unexpectedly: {err}"));
            }
        }
    }
}

fn read_output(child: &mut Child) -> (String, String) {
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_string(&mut stdout);
    }
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    (stdout, stderr)
}

fn word_error(stderr: &str, stdout: &str) -> String {
    let combined = format!("{stderr}\n{stdout}");
    let lower = combined.to_lowercase();
    if lower.contains("80040154")
        || lower.contains("class not registered")
        || lower.contains("com class factory")
        || lower.contains("cannot create")
    {
        return "Microsoft Word could not be started. Word is required for Word to PDF conversion; install Microsoft Word and try again."
            .into();
    }
    if lower.contains("password") || lower.contains("encrypted") {
        return "This Word document is password-protected. Remove the password in Word, then try again.".into();
    }
    if lower.contains("cannot open") || lower.contains("could not be found") || lower.contains("not found") {
        return "Microsoft Word could not open this document. Make sure it is a valid Word file and is not locked by another program."
            .into();
    }
    let detail = combined
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .last()
        .unwrap_or("Unknown error");
    format!("Microsoft Word could not convert this document: {detail}")
}

fn sanitize_text(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t' | '\r'))
        .map(|ch| match ch {
            '\t' => ' ',
            '\r' => '\n',
            other => other,
        })
        .collect()
}

fn unique_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{}-{}", std::process::id(), nanos)
}

fn word_pids() -> Vec<u32> {
    let output = match Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq WINWORD.EXE", "/FO", "CSV", "/NH"])
        .output()
    {
        Ok(output) => output,
        Err(_) => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(',').collect();
            if fields.len() < 2 {
                return None;
            }
            fields[1].trim().trim_matches('"').parse::<u32>().ok()
        })
        .collect()
}

fn kill_new_word_processes(before: &[u32]) {
    for pid in word_pids() {
        if !before.contains(&pid) {
            let _ = Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/F", "/T"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_docx_from_extracted_text() {
        let payload = r#"{"pages":[["Hello world\nSecond line","Another paragraph"],["Page two"]]}"#;
        let output = std::env::temp_dir().join(format!("pdfzen-text-test-{}.docx", unique_id()));
        text_to_word(payload, output.to_str().unwrap()).expect("docx build");
        let bytes = fs::read(&output).expect("read docx");
        assert!(bytes.starts_with(b"PK"), "DOCX should be a zip archive");
        assert!(bytes.len() > 1_000, "DOCX should contain document parts");
        let _ = fs::remove_file(&output);
    }
}
