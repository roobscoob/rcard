pub use tfw::elf_cache::Backtrace;

use super::{DIM, RESET};

pub fn print_backtrace(bt: &Backtrace, _task_pad: usize, term_width: usize, pw: usize) {
    let cont_prefix = format!("{:>width$} {DIM}|{RESET} ", "", width = pw - 3,);
    let content_width = term_width.saturating_sub(pw);

    println!("{cont_prefix}{DIM}backtrace:{RESET}");

    // skip the first frame, since it's rcade_log::stack_dump::capture() itself
    for frame in bt.frames.iter().skip(1) {
        let inline_tag = if frame.is_inline { " [inline]" } else { "" };
        let loc = match (&frame.file, frame.line) {
            (Some(file), Some(line)) => format!("{file}:{line}"),
            (Some(file), None) => file.clone(),
            _ => String::new(),
        };

        let name = &frame.function;
        let left = format!("  {name}{inline_tag}");
        if !loc.is_empty() {
            let pad = content_width.saturating_sub(left.len() + loc.len() + 1);
            println!("{cont_prefix}{left}{:pad$} {DIM}{loc}{RESET}", "");
        } else {
            println!("{cont_prefix}{left}");
        }
    }
}
