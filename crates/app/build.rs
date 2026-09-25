fn main() {
    // tauri-build embeds icons/icon.ico into the executable but does not tell
    // cargo to watch it, so a changed icon is ignored until something else
    // forces the build script to run again.
    println!("cargo:rerun-if-changed=icons");
    tauri_build::build()
}
