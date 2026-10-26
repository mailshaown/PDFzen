fn main() {
    // Windows GNU builds need windres to compile the application icon resources.
    // The local toolchain here does not ship binutils, so use the small resource
    // toolchain vendored under tools/windres when it is present.
    let resource_tools = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tools")
        .join("windres");

    if resource_tools.is_dir() {
        if let Some(path) = std::env::var_os("PATH") {
            let mut paths = vec![resource_tools.clone()];
            paths.extend(std::env::split_paths(&path));
            if let Ok(joined) = std::env::join_paths(paths) {
                // SAFETY: the build script is single-threaded at this point and no
                // other code is reading the environment concurrently.
                unsafe {
                    std::env::set_var("PATH", joined);
                }
            }
        }
        println!("cargo:rerun-if-changed=tools/windres");
    }

    tauri_build::build()
}
