// A manifestet (Per-Monitor V2 DPI, Common Controls v6) közvetlenül az MSVC linker ágyazza be,
// így nem kell hozzá rc.exe vagy külön build crate. Az exe ikon és a verzióinfó később jön.
fn main() {
    let manifest = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("res/napcat.manifest");
    println!("cargo:rerun-if-changed=res/napcat.manifest");
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", manifest.display());
}
