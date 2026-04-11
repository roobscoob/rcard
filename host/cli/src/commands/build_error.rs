use tfw::build::BuildError;

pub fn render(error: &BuildError) {
    eprintln!("\n{error:?}\n");
}
