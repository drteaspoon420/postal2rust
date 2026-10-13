//! `nav_dump <map.fuk> [--unchained]`: prints a map's navigation points and edges.

use p2nav::NavGraph;
use std::collections::BTreeMap;
use upkg::Package;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let pkg = Package::open(&a[0]).expect("map");
    let g = NavGraph::from_package(&pkg);
    let unchained = a.iter().any(|x| x == "--unchained");
    let mut by: BTreeMap<(String, bool, bool), usize> = BTreeMap::new();
    for n in &g.nodes {
        *by.entry((n.class.clone(), n.in_chain, !n.paths.is_empty())).or_default() += 1;
        if unchained && !n.in_chain {
            println!("{:<20} {:<24} paths {:>2} next {:?} loc {:?}", n.class, n.name, n.paths.len(), n.next.map(|k| &g.nodes[k as usize].name), n.location);
        }
    }
    if a.iter().any(|x| x == "--chain") {
        let mut cur = g.head;
        while let Some(k) = cur {
            let n = &g.nodes[k as usize];
            println!("chain {:<20} {:<24} flags {:#010x} paths {}", n.class, n.name, pkg.exports[n.export].flags, n.paths.len());
            cur = n.next;
            if !n.in_chain {
                break;
            }
        }
    }
    let mut flags: BTreeMap<(bool, u32), usize> = BTreeMap::new();
    for n in &g.nodes {
        *flags.entry((n.in_chain, pkg.exports[n.export].flags)).or_default() += 1;
    }
    println!("(chained, export flags) -> count: {flags:x?}");
    println!("(class, chained, has paths) -> count");
    for (k, v) in by {
        println!("  {v:>6}  {k:?}");
    }
}
