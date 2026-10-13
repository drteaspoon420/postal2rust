//! `nav_scan [maps dir]`: decodes the navigation network of every map and prints counts.
//! Exits non-zero if any map fails to open or has inconsistent references.

use p2nav::NavGraph;
use std::collections::BTreeMap;
use upkg::Package;

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| {
        let root = std::env::var("POSTAL2_DIR").unwrap_or_else(|_| r"G:\SteamLibrary\steamapps\common\POSTAL2Complete".into());
        format!(r"{root}\Maps")
    });
    let mut files: Vec<_> = std::fs::read_dir(&dir).expect("maps dir").flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("fuk"))).collect();
    files.sort();
    let (mut maps, mut failed, mut nodes, mut specs, mut chained) = (0, 0, 0usize, 0usize, 0usize);
    let mut classes: BTreeMap<String, usize> = BTreeMap::new();
    let mut flags: BTreeMap<i32, usize> = BTreeMap::new();
    let mut problems = 0usize;
    let mut no_nav = 0;
    for p in &files {
        let name = p.file_stem().unwrap().to_string_lossy().to_string();
        let pkg = match Package::open(p) {
            Ok(p) => p,
            Err(e) => {
                println!("{name:<28} OPEN FAILED {e}");
                failed += 1;
                continue;
            }
        };
        maps += 1;
        let g = NavGraph::from_package(&pkg);
        let s = &g.stats;
        let bad = s.property_failures + s.bad_refs + s.mismatched_paths;
        problems += bad;
        if g.nodes.is_empty() {
            no_nav += 1;
        }
        let edges: usize = g.nodes.iter().map(|n| n.paths.len()).sum();
        println!(
            "{name:<28} points {:>5} (chained {:>5}, unchained w/ paths {:>3})  reachspecs {:>6} (in PathLists {:>6}, orphan {:>4})  bad {bad}",
            g.nodes.len(),
            s.chained,
            s.unchained_points,
            s.reach_specs,
            edges,
            s.orphan_specs
        );
        nodes += g.nodes.len();
        specs += s.reach_specs;
        chained += s.chained;
        for (c, n) in g.class_counts() {
            *classes.entry(c).or_default() += n;
        }
        for sp in &g.specs {
            *flags.entry(sp.reach_flags).or_default() += 1;
        }
    }
    println!("\nNAVSCAN: maps={maps} open failures={failed} without nav={no_nav}; points={nodes} (chained {chained}) reachspecs={specs}; reference problems={problems}");
    println!("point classes:");
    let mut cv: Vec<_> = classes.into_iter().collect();
    cv.sort_by(|a, b| b.1.cmp(&a.1));
    for (c, n) in cv {
        println!("  {n:>7}  {c}");
    }
    println!("reachFlags values:");
    for (f, n) in flags {
        println!("  {n:>7}  {f:#06x}");
    }
    if failed > 0 || problems > 0 {
        std::process::exit(1);
    }
}
