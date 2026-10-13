//! `usscan [dir]` : parses every script export in every `.u` package under the directory
//! (default: `$POSTAL2_DIR/System`) and decodes all bytecode, reporting totals and failures.

use std::collections::{BTreeMap, HashMap, HashSet};
use upkg::Package;
use uscript::bytecode::{Expr, walk};
use uscript::objects::{ScriptObject, is_script_class, parse_export};

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| {
        let root = std::env::var("POSTAL2_DIR").unwrap_or_else(|_| r"G:\SteamLibrary\steamapps\common\POSTAL2Complete".into());
        format!(r"{root}\System")
    });
    let verbose = std::env::args().any(|a| a == "-v");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("read dir")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("u")))
        .collect();
    files.sort();
    let (mut pkgs, mut objs, mut obj_fail) = (0usize, 0usize, 0usize);
    let (mut funcs, mut func_bytes, mut stmts, mut scripts, mut script_bytes) = (0usize, 0usize, 0usize, 0usize, 0usize);
    let mut per_kind: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    // Native index -> declared function name; indices called from bytecode.
    let mut native_names: HashMap<u16, String> = HashMap::new();
    let mut called: HashSet<u16> = HashSet::new();
    // Every native function declaration ("Package.Class.Function"), indexed or not.
    let mut native_funcs: std::collections::BTreeSet<String> = Default::default();
    for p in &files {
        let pkg = match Package::open(p) {
            Ok(p) => p,
            Err(e) => {
                println!("{}: OPEN FAILED {e}", p.display());
                continue;
            }
        };
        pkgs += 1;
        let mut bad = 0;
        let (mut pf, mut pb) = (0, 0);
        for i in 0..pkg.exports.len() {
            let class = pkg.export_class(i).to_string();
            if !is_script_class(&class) {
                continue;
            }
            objs += 1;
            let entry = per_kind.entry(class.clone()).or_default();
            entry.0 += 1;
            match parse_export(&pkg, i) {
                Ok(Some(o)) => {
                    if let Some(s) = o.struct_data() {
                        for st in &s.script.stmts {
                            walk(&st.expr, &mut |e| {
                                if let Expr::NativeCall { index, .. } = e {
                                    called.insert(*index);
                                }
                            });
                        }
                        if s.script_size > 0 {
                            scripts += 1;
                            script_bytes += s.script_size as usize;
                            stmts += s.script.stmts.len();
                        }
                    }
                    if let ScriptObject::Function(f) = &o {
                        if f.native != 0 {
                            native_names.entry(f.native).or_insert_with(|| pkg.name(pkg.exports[i].name).to_string());
                        }
                        if f.flags & uscript::objects::func_flags::NATIVE != 0 {
                            let owner = pkg.ref_name(pkg.exports[i].outer).to_string();
                            native_funcs.insert(format!("{}.{owner}.{}", pkg.name, pkg.name(pkg.exports[i].name)));
                        }
                        funcs += 1;
                        func_bytes += f.base.script_size as usize;
                        pf += 1;
                        pb += f.base.script_size as usize;
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    entry.1 += 1;
                    bad += 1;
                    if bad <= 3 || verbose {
                        println!("  {} ({class}): {e}", pkg.full_path(upkg::ObjRef::Export(i)));
                    }
                }
            }
        }
        obj_fail += bad;
        println!("{:<28} functions={pf:<6} bytecode={pb:<8} failures={bad}", pkg.name);
    }
    println!();
    for (k, (n, f)) in &per_kind {
        println!("  {k:<20} {n:>7} parsed, {f} failed");
    }
    println!(
        "USSCAN: packages={pkgs} script objects={objs} failures={obj_fail}; functions={funcs} function bytecode={func_bytes} bytes; \
         all scripts={scripts} ({script_bytes} bytes, {stmts} statements)"
    );
    // Native coverage of the interpreter (only natives the VM can resolve need implementing here;
    // the VM also needs a System dir, so this is skipped when Core.u is absent).
    if let Ok(vm) = uscript::vm::Vm::new(&dir) {
        let unresolved = called.iter().filter(|i| !native_names.contains_key(i)).count();
        let implemented = called.iter().filter(|i| native_names.get(i).is_some_and(|n| vm.has_native(n))).count();
        println!(
            "NATIVES: {} declared with an index, {} distinct indices called from bytecode ({unresolved} unresolved), {implemented} implemented in the VM",
            native_names.len(),
            called.len()
        );
        let total = native_funcs.len();
        let fname = |n: &str| n.rsplit('.').next().unwrap_or(n).to_string();
        let have = native_funcs.iter().filter(|n| vm.has_native(&fname(n))).count();
        let core: Vec<_> = native_funcs.iter().filter(|n| n.to_ascii_lowercase().starts_with("core.")).collect();
        let core_have = core.iter().filter(|n| vm.has_native(&fname(n))).count();
        println!("NATIVE DECLS: {have}/{total} native functions implemented (Core: {core_have}/{})", core.len());
        if std::env::args().any(|a| a == "--missing") {
            let mut idx: Vec<_> = called.iter().filter_map(|i| native_names.get(i).filter(|n| !vm.has_native(n)).map(|n| (*i, n))).collect();
            idx.sort();
            for (i, n) in idx {
                println!("  missing indexed native({i}) {n}");
            }
            for n in &native_funcs {
                if !vm.has_native(&fname(n)) {
                    println!("  missing {n}");
                }
            }
        }
    }
    if obj_fail > 0 {
        std::process::exit(1);
    }
}
