#[cfg(windows)]
fn main() {
    let mut resource = winresource::WindowsResource::new();
    resource.set_icon("assets/icon.ico");
    resource.set("FileDescription", "VideoSniffer");
    resource.set("ProductName", "VideoSniffer");
    resource.set("OriginalFilename", "VideoSniffer.exe");

    if let Err(error) = resource.compile() {
        panic!("failed to embed Windows resources: {error}");
    }
}

#[cfg(not(windows))]
fn main() {}
