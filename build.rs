// Windows erőforrások: exe ikon, verzióinfó (a Cargo.toml verziójából) és a manifest
// (Per-Monitor V2 DPI, Common Controls v6). A Windows SDK rc.exe-jét használja.
fn main() {
    println!("cargo:rerun-if-changed=res/napcat.manifest");
    println!("cargo:rerun-if-changed=res/napcat.ico");
    let mut res = winresource::WindowsResource::new();
    res.set_icon("res/napcat.ico")
        .set_manifest_file("res/napcat.manifest")
        .set("ProductName", "NapCat")
        .set("FileDescription", "NapCat")
        .set("CompanyName", "Cat Ninth");
    res.compile().expect("Windows erőforrások fordítása (rc.exe)");
}
