//! Histogram of export classes over every package in the given directories.
//! Run: cargo run --release -p upkg --example class_hist -- <dir>...
use std::collections::BTreeMap;
use upkg::Package;

fn main() {
    let mut hist: BTreeMap<String, usize> = BTreeMap::new();
    for d in std::env::args().skip(1) {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let Ok(pkg) = Package::open(e.path()) else { continue };
            for i in 0..pkg.exports.len() {
                *hist.entry(pkg.export_class(i).to_string()).or_default() += 1;
            }
        }
    }
    let mut v: Vec<_> = hist.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    for (c, n) in v {
        println!("{n:8} {c}");
    }
}
