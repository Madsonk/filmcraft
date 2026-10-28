//! Translate lines with an Opus-MT model directory: `cargo run --release -p filmcraft-speech --features translate --example translate -- <model dir> "line" …`
//! (no lines: read standard input, one per line).

use std::io::BufRead;

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("model dir"));
    let t0 = std::time::Instant::now();
    let tr = filmcraft_speech::mt::Translator::load(&dir).expect("load");
    eprintln!("loaded in {:.2}s", t0.elapsed().as_secs_f64());
    let mut lines: Vec<String> = args.collect();
    if lines.is_empty() {
        lines = std::io::stdin().lock().lines().map_while(Result::ok).collect();
    }
    for l in lines {
        let t = std::time::Instant::now();
        let out = tr.translate(&l, 4).expect("translate");
        println!("{l}\n  -> {out}   [{:.2}s]", t.elapsed().as_secs_f64());
    }
}
