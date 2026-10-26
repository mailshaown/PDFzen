# PDFzen

A Windows desktop PDF viewer/editor built with **Tauri 2**, **Rust** and a
vanilla-JS front end. PDFs are rendered with a vendored copy of **pdf.js**, and
edits are composed into real PDF objects by a Rust engine using **lopdf**.

## Features

### Reading

- Continuous, lazily rendered pages with zoom, fit-to-width and a page-number
  status bar.
- Selectable text layer, resizable thumbnail strip, document bookmarks (outline)
  and a comments pane. Drag the divider beside the pages panel to widen it up to
  half the window; double-click it to reset. Selecting a thumbnail, bookmark,
  comment or search match scrolls the viewer to that page and renders it
  immediately.
- Select text in Select mode to reveal floating **Highlight** and **Comment**
  actions. Multi-line selections become one highlight with per-line rectangles,
  and a comment can be attached directly to that highlight.
- Full-document text search with exact-word highlighting and previous/next
  navigation.
- Drag and drop a PDF (or images) onto the window to open/append.
- Light, Dark and Auto themes; Auto follows the Windows/device theme and updates
  immediately when it changes. The choice is remembered between launches.
- Print and keyboard shortcuts.

### Annotate

Highlight, underline, strikeout, squiggly, freehand pen, rectangle, typed text,
sticky notes, stamps, signature images, and black redaction boxes. Choose a
colour and stroke width; erase annotations with the eraser. Text boxes are
committed with `Ctrl+Enter`, by clicking outside, or by switching tools. The
annotation toolbar is icon-based; hover a tool to see its name and shortcut. The
pen tool uses a pen-shaped cursor, the eraser uses an eraser cursor, and the text
tool uses a text cursor.

The Sign button offers **Type to sign** with four signature styles and an ink
colour, plus the original freehand signature pad. A typed signature is rendered
to a transparent PNG and placed on the page like a drawn signature. Add and
delete annotations, edit highlight comments, and add or delete bookmarks for the
current page from the Comments and Bookmarks panes. Highlight comments are
written to the PDF's `/Contents` entry and are imported again when the saved
document is reopened. Bookmarks are written into the saved PDF's outline.

Annotations are written to the saved PDF as genuine annotation objects
(`/Highlight`, `/Underline`, `/StrikeOut`, `/Squiggly`, `/Ink`, `/Square`,
`/FreeText`, `/Text`, `/Stamp`) with `/AP` appearance streams, so they appear in
Acrobat, Edge, Chrome and other viewers. Signature PNGs are embedded losslessly
with an `/SMask` alpha channel; JPEG signatures use `DCTDecode`.

### Organize pages

Rotate, delete, move up/down, extract a page, insert a blank page, reverse the
document, split every page into its own PDF, append pages from other PDFs
(merge), and create a PDF from images. Use **+ Blank** with no document open to
start a new blank PDF. Crop pages visually. Add or remove
bookmarks in the Bookmarks pane; saved bookmarks are emitted as a real PDF
outline with clickable destinations. Add comments as sticky-note annotations
and delete them from the Comments pane.

### Save, secure and export

- Save in place or Save as, with undo/redo for page and annotation edits.
- Optionally protect saved documents with an AES-256 open password and
  print/copy/modify/annotate/assemble permissions. Protected files carry a
  PDFzen tamper-evidence seal; it is not a certificate-based digital signature.
- Flatten annotations into page content.
- Add a configurable text watermark with opacity and angle.
- Add `n / total` page numbers (kept inside the visible crop area).
- Remove document metadata, or set title/author/subject/keywords.
- Black-box redaction overlay (see limitations).
- Print, export page text to `.txt`, export the current page to PNG, and extract
  embedded raster images to a folder.

## Keyboard shortcuts

| Shortcut | Action |
| --- | --- |
| `Ctrl+O` / `Ctrl+S` / `Ctrl+Shift+S` | Open / Save / Save as |
| `Ctrl+P` | Print |
| `Ctrl+Z` / `Ctrl+Y` (or `Ctrl+Shift+Z`) | Undo / Redo |
| `Ctrl+0` / `Ctrl++` / `Ctrl+-` | Fit width / zoom in / zoom out |
| `V` `H` `U` `K` `J` `P` `R` `T` `N` `M` `X` `C` `E` | Select, highlight, underline, strikeout, squiggly, pen, rectangle, text, note, stamp, redact, crop, erase |
| `←` / `→` (`PageUp` / `PageDown`, `Space`) | Previous / next page |
| `Delete` / `Backspace` | Delete current page |
| `Ctrl+Enter` | Commit a text box, sticky note or highlight comment while editing |
| `Escape` | Cancel a text box or comment, or close a modal/menu |

## How saving works

The front end keeps a list of `{ document, page index, rotation, annotations }`
entries. On save it sends the original PDF bytes plus that list to the Rust
command `build_pdf`, which:

1. deep-copies each requested page (content, resources, media box),
2. bakes the display rotation into the page content (`q … cm` wrapper) and
   resets `/Rotate` to 0,
3. applies a `/CropBox` for cropped pages and positions page furniture inside it,
4. rebuilds the page tree and catalog,
5. adds annotation dictionaries with appearance streams, or flattens them into
   page content, and
6. adds watermarks, page numbers and document information.

The result is written with `save_pdf`, either over the opened file or to a new
location. All content streams are written as indirect objects; this matters
because the PDF writer silently drops direct streams inside a `/Contents` array.

## Project layout

```
src/                 front end (index.html, main.js, styles.css)
src/vendor/          vendored pdf.js 3.11 UMD build + worker (offline)
src-tauri/src/pdf.rs PDF composition engine
src-tauri/src/main.rs Tauri commands (open_pdf, save_pdf, build_pdf, …) + app
src-tauri/examples/make_sample.rs  generates sample.pdf
sample.pdf           demo document with text and sample annotations
```

## Development

```powershell
npm install
npm run tauri dev      # run the app
npm run tauri build    # build the installer
```

Rust tests and the sample document:

```powershell
cd src-tauri
cargo test
cargo run --example make_sample
```

### Windows notes

- The build targets the **GNU** toolchain (`stable-x86_64-pc-windows-gnu`) so
  no Visual Studio/UAC setup is required.
- `src-tauri/.cargo/config.toml` redirects `target-dir` to `C:/pdfed-target`
  because the project path contains a space, which breaks `windres`.
- A harmless linker warning (`.rsrc merge failure: multiple non-default
  manifests`) is printed on every GNU link.
- Run `cargo` from `src-tauri` so the `.cargo/config.toml` target-dir applies.

## Limitations

- **Redaction is visual only.** Black boxes cover page content but do not remove
  it from the PDF; do not rely on them for confidential data. True content
  removal and sanitisation are not implemented.
- **No AcroForm support**: interactive form fields can be viewed but not filled.
- **No OCR, Word/Office export, or in-place text/image editing**. Existing
  highlight, underline, strikeout, squiggly, sticky-note, free-text and square
  annotations are imported for viewing and editing. Imported Ink and Stamp
  annotations are not reconstructed in the editor.
- **Sticky notes are dropped when flattening** because they have no appearance
  stream. Keep flattening off to preserve `/Text` note annotations.
- **Imported annotations are re-created when saving.** The editor keeps imported
  highlights and comments as editable session objects; the original annotation
  identity and replies are not preserved.
- Search matches are found within individual pdf.js text items, so a phrase
  split across several items is not matched as a whole.
- Thumbnails are re-rendered from the source page; only page-level annotation
  overlays are drawn on thumbnails, not at full fidelity.
