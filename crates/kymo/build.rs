//! On Windows, links the icon and version information into kymo.exe as resources, which
//! Explorer, the Start menu, the taskbar and the installer's shortcuts show.

fn main() {
    println!("cargo::rerun-if-changed=assets/kymo.ico");
    println!("cargo::rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut resource = winresource::WindowsResource::new();
    resource
        .set_icon("assets/kymo.ico")
        .set("ProductName", "Kymo")
        .set("FileDescription", "Kymo")
        .set("LegalCopyright", "© Kymo developers");
    if let Err(error) = resource.compile() {
        panic!("cannot link the icon and version into kymo.exe: {error}");
    }
}
