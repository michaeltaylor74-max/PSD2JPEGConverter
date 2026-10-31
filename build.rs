// Gives the Windows .exe its icon and file details (shown in Explorer).
fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico")
            .set("ProductName", "PSD to JPEG Converter")
            .set("FileDescription", "PSD to JPEG Converter");
        res.compile().expect("could not embed the Windows icon");
    }
}
