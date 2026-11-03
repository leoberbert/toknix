// A Windows release is a GUI-subsystem program, so opening the file from
// Explorer does not leave a command window behind. A parent console is
// attached when the program is started from one.
// Release builds hide the console. Test builds stay console programs so
// `cargo test` can run the harness.
#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

#[tokio::main]
async fn main() {
    #[cfg(windows)]
    attach_parent_console();
    if let Err(error) = toknix::run().await {
        eprintln!("toknix: {error}");
        std::process::exit(1);
    }
}

#[cfg(windows)]
fn attach_parent_console() {
    use std::fs::OpenOptions;
    use std::os::windows::io::AsRawHandle;

    const ATTACH_PARENT_PROCESS: u32 = 0xFFFF_FFFF;
    const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5;
    const STD_ERROR_HANDLE: u32 = 0xFFFF_FFF4;

    unsafe extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
        fn SetStdHandle(std_handle: u32, handle: *mut core::ffi::c_void) -> i32;
    }

    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return;
        }
        let Ok(output) = OpenOptions::new().read(true).write(true).open("CONOUT$") else {
            return;
        };
        let handle = output.as_raw_handle() as *mut core::ffi::c_void;
        SetStdHandle(STD_OUTPUT_HANDLE, handle);
        SetStdHandle(STD_ERROR_HANDLE, handle);
        std::mem::forget(output);
    }
}
