//! Windows: embeds Irori's icon and version details in `irori-sign-in.exe`, so
//! Explorer and the file's properties show them, and signing services can
//! read who the program is from.

fn main() {
    #[cfg(windows)]
    windows::embed("irori-sign-in.exe", "Irori store sign-in window");
}

#[cfg(windows)]
mod windows {
    pub fn embed(file_name: &str, description: &str) {
        println!("cargo:rerun-if-changed=../../packaging/irori.ico");
        let mut resource = winresource::WindowsResource::new();
        // FileVersion and ProductVersion come from the package version.
        resource
            .set_icon("../../packaging/irori.ico")
            .set("ProductName", "Irori")
            .set("FileDescription", description)
            .set("CompanyName", "Irori developers")
            .set("LegalCopyright", "Irori developers. MIT License.")
            .set("OriginalFilename", file_name)
            .set("InternalName", file_name);
        resource
            .compile()
            .expect("the Windows SDK's resource compiler embeds the icon");
    }
}
