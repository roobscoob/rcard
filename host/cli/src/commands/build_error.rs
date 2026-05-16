use console::style;
use tfw::build::BuildError;

pub fn render(error: &BuildError) {
    eprintln!();
    let msg = format!("{error:?}");
    for line in msg.lines() {
        eprintln!("    {}", style(line).red());
    }
    eprintln!();
}
