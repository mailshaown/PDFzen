# Local Windows resource toolchain

This folder contains the minimum tools needed to embed PDFzen's Windows icon
when building with the `x86_64-pc-windows-gnu` Rust toolchain on this machine.
The installed Rust GNU toolchain does not include binutils, so `tauri-build`
cannot find `windres` on its own.

Included:

- `windres.exe` and its runtime DLLs from MinGW-w64 binutils.
- `gcc.exe`, a small wrapper used only because windres invokes a preprocessor.
  For `-E` it copies the resource script through unchanged; Tauri's generated
  `resource.rc` has no includes or macros.

`build.rs` prepends this folder to `PATH` while running the Tauri build script.
If a full MinGW-w64 toolchain is installed later, this folder can be removed and
the system `windres`/`gcc` can be used instead.
