use clap::Parser;

fn main() {
    let args = mangadex_dl::cli::Args::parse();
    let code = if args.wants_window() {
        // Сборка остаётся консольной, чтобы --cli печатал в существующий терминал.
        // Своё окно консоли прячем только когда программу открыли двойным щелчком.
        #[cfg(windows)]
        hide_owned_console();
        match mangadex_dl::gui::run(&args.lang, &args.out, args.saver) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("Ошибка: {error}");
                1
            }
        }
    } else {
        mangadex_dl::cli::execute(args)
    };
    std::process::exit(code);
}

#[cfg(windows)]
fn hide_owned_console() {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetConsoleWindow() -> *mut core::ffi::c_void;
        fn GetConsoleProcessList(process_list: *mut u32, process_count: u32) -> u32;
    }
    #[link(name = "user32")]
    extern "system" {
        fn ShowWindow(hwnd: *mut core::ffi::c_void, cmd: i32) -> i32;
    }
    unsafe {
        let mut processes = [0u32; 16];
        let owners = GetConsoleProcessList(processes.as_mut_ptr(), processes.len() as u32);
        if owners == 1 {
            let window = GetConsoleWindow();
            if !window.is_null() {
                ShowWindow(window, 0);
            }
        }
    }
}
