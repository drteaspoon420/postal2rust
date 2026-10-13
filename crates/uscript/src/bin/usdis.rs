//! `usdis <package.u|name> [Class] [--func Name]`
//!
//! Prints a class (or every class in the package) as pseudo-code reconstructed from the
//! compiled bytecode. A bare package name is looked up under `$POSTAL2_DIR/System`.
//! Native function names/operators are resolved from Core and Engine declarations.

use std::path::PathBuf;
use uscript::disasm::Disasm;
use uscript::graph::{NativeTable, ScriptPackage};
use uscript::objects::ScriptObject;

fn system_dir() -> PathBuf {
    let root = std::env::var("POSTAL2_DIR").unwrap_or_else(|_| r"G:\SteamLibrary\steamapps\common\POSTAL2Complete".into());
    PathBuf::from(root).join("System")
}

fn open(name: &str) -> ScriptPackage {
    let p = PathBuf::from(name);
    let path = if p.is_file() { p } else { system_dir().join(format!("{name}.u")) };
    ScriptPackage::open(&path).unwrap_or_else(|e| {
        eprintln!("{}: {e}", path.display());
        std::process::exit(1)
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(pkg_name) = args.first() else {
        eprintln!("usage: usdis <package> [Class] [--func Name]");
        std::process::exit(2);
    };
    let func = args.iter().position(|a| a == "--func").and_then(|i| args.get(i + 1)).cloned();
    let class = args.get(1).filter(|a| !a.starts_with("--")).cloned();

    let sp = open(pkg_name);
    let mut natives = NativeTable::default();
    natives.add_package(&sp);
    for base in ["Core", "Engine"] {
        if !sp.pkg.name.eq_ignore_ascii_case(base) {
            natives.add_package(&open(base));
        }
    }
    let d = Disasm { sp: &sp, natives: &natives };

    let classes: Vec<usize> = match &class {
        Some(c) => vec![sp.find_class(c).unwrap_or_else(|| {
            eprintln!("class {c} not found in {}", sp.pkg.name);
            std::process::exit(1)
        })],
        None => (0..sp.objects.len()).filter(|&i| sp.class(i).is_some()).collect(),
    };
    for c in classes {
        match &func {
            None => println!("{}", d.class(c)),
            Some(f) => {
                // Search class functions and functions inside states.
                let mut found = false;
                for k in sp.children(c) {
                    let mut cands = vec![k];
                    if matches!(sp.objects[k], Some(ScriptObject::State(_))) {
                        cands.extend(sp.children(k));
                    }
                    for m in cands {
                        if let Some(fd) = sp.function(m) {
                            if sp.export_name(m).eq_ignore_ascii_case(f) {
                                let owner = sp.pkg.full_path(upkg::ObjRef::Export(m));
                                println!("// {owner}  (ScriptSize {} bytes)", fd.base.script_size);
                                println!("{}", d.function(m, fd, ""));
                                found = true;
                            }
                        }
                    }
                }
                if !found {
                    eprintln!("function {f} not found");
                }
            }
        }
    }
}
