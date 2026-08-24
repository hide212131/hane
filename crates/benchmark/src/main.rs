use hane_benchmark::{
    Environment, generate_fixtures, markdown_report, run_buffer_edit_scenario,
    run_file_open_scenario, run_layout_scenario, run_presentation_scenario,
};
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match std::env::args().nth(1).as_deref() {
        Some("fixtures") => {
            for path in generate_fixtures(Path::new("target/fixtures"))? {
                println!("{}", path.display());
            }
        }
        Some("buffer") => {
            let ten = run_buffer_edit_scenario(10 * 1024 * 1024, 90);
            let hundred = run_buffer_edit_scenario(100 * 1024 * 1024, 90);
            let open_ten =
                run_file_open_scenario(Path::new("target/fixtures/markdown_10mb.md"), 30)?;
            let open_hundred =
                run_file_open_scenario(Path::new("target/fixtures/markdown_100mb.md"), 30)?;
            let presentation = run_presentation_scenario(1_000);
            let layout = run_layout_scenario(100_000, 1_000);
            print!(
                "{}",
                markdown_report(
                    &Environment::collect("release"),
                    &[
                        ("10 MB buffer edit", ten),
                        ("100 MB buffer edit", hundred),
                        ("10 MB file open", open_ten),
                        ("100 MB file open", open_hundred),
                        ("bold presentation update", presentation),
                        ("visible layout index", layout),
                    ]
                )
            );
        }
        _ => {
            eprintln!("usage: hane-bench <fixtures|buffer>");
            std::process::exit(2);
        }
    }
    Ok(())
}
