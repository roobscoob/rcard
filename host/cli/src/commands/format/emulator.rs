pub fn unsupported() -> ! {
    eprintln!("error: the `format` command is not supported with the emulator backend");
    std::process::exit(1);
}
