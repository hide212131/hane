#![allow(
    clippy::items_after_statements,
    reason = "the CLI-local report helper stays next to its only invocation"
)]

use hane_benchmark::{
    Environment, generate_fixtures, markdown_report, run_block_heights_scenario,
    run_block_index_scenario, run_block_layout_scenario, run_buffer_edit_scenario,
    run_file_open_scenario, run_height_splice_scenario, run_layout_scenario,
    run_presentation_scenario,
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
            let one = run_buffer_edit_scenario(1024 * 1024, 90);
            let ten = run_buffer_edit_scenario(10 * 1024 * 1024, 90);
            let hundred = run_buffer_edit_scenario(100 * 1024 * 1024, 90);
            let open_one =
                run_file_open_scenario(Path::new("target/fixtures/markdown_1mb.md"), 30)?;
            let open_ten =
                run_file_open_scenario(Path::new("target/fixtures/markdown_10mb.md"), 30)?;
            let open_hundred =
                run_file_open_scenario(Path::new("target/fixtures/markdown_100mb.md"), 30)?;
            let presentation = run_presentation_scenario(1_000);
            let layout = run_layout_scenario(100_000, 1_000);
            const BLOCK_INDEX_EDITS: usize = 200;
            let typing_index = run_block_index_scenario(100_000, BLOCK_INDEX_EDITS, false);
            let structural_index = run_block_index_scenario(100_000, BLOCK_INDEX_EDITS, true);
            let block_heights = run_block_heights_scenario(100_000, 30);
            let height_splice = run_height_splice_scenario(100_000, 200);
            let block_layout = run_block_layout_scenario(100_000, 200);
            print!(
                "{}",
                markdown_report(
                    &Environment::collect("release"),
                    &[
                        ("1 MB buffer edit", one),
                        ("10 MB buffer edit", ten),
                        ("100 MB buffer edit", hundred),
                        ("1 MB file open", open_one),
                        ("10 MB file open", open_ten),
                        ("100 MB file open", open_hundred),
                        ("Markdown presentation update", presentation),
                        ("visible layout index", layout),
                        ("block index update while typing", typing_index.update),
                        (
                            "block index update splitting a block",
                            structural_index.update
                        ),
                        ("block height index rebuild", block_heights),
                        ("block height index local splice", height_splice),
                        ("viewport block layout", block_layout),
                    ]
                )
            );
            println!(
                "- Block index re-parse: at most {} bytes while typing, {} bytes when splitting a block",
                typing_index.max_reparsed_bytes, structural_index.max_reparsed_bytes,
            );
            println!(
                "- Block index invalidation: {} blocks over {} local edits",
                typing_index.invalidated_blocks + structural_index.invalidated_blocks,
                2 * BLOCK_INDEX_EDITS,
            );
        }
        _ => {
            eprintln!("usage: hane-bench <fixtures|buffer>");
            std::process::exit(2);
        }
    }
    Ok(())
}
