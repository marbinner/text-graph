//! Print every character `mathtext` can draw — the inventory
//! `assets/gen-math-font.sh` subsets `assets/math.ttf` down to, and the
//! same list `mathtext::glyphs()` hands the app's font-coverage test.
//! Deriving the codepoints instead of listing them is what keeps the
//! bundled font and the symbol tables from drifting apart.
fn main() {
    println!("{}", text_graph::mathtext::glyphs());
}
