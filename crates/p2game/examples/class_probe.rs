//! Research probe: resolves a class through the VM and evaluates a cast chain the way
//! `GameInfo.Login` does. Usage: class_probe Package.Class [Base.Class]
use uscript::vm::{Value, Vm};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let root = p2game::game_root(None);
    let mut vm = Vm::new(root.join("System")).unwrap();
    let name = a.first().cloned().unwrap_or_else(|| "GameTypes.AWDudePlayer".into());
    let (p, c) = name.split_once('.').unwrap();
    match vm.find_class(p, c) {
        Ok(id) => {
            let mut chain = vec![];
            let mut cur = Some(id);
            while let Some(k) = cur {
                chain.push(format!("{}.{}{}", vm.classes[k].package, vm.classes[k].name, if vm.classes[k].is_abstract() { "(abstract)" } else { "" }));
                cur = vm.classes[k].super_;
            }
            println!("{name}: {}", chain.join(" <- "));
            // `class_probe Package.Class - Prop`: print a class default.
            if let Some(prop) = a.get(2) {
                println!("default {prop} = {:?}", vm.default_value(id, prop));
            }
            if let Some(base) = a.get(1).filter(|b| *b != "-") {
                let (bp, bc) = base.split_once('.').unwrap();
                let b = vm.find_class(bp, bc).unwrap();
                println!("is child of {base}: {}", vm.is_child_of(id, b));
            }
        }
        Err(e) => println!("{name}: {e}"),
    }
    let obj = vm.find_class("Core", "Object").unwrap();
    let class_class = vm.find_class("Core", "Class").ok();
    let r = vm.call(obj, None, "DynamicLoadObject", vec![Value::Str(name.clone()), Value::Class(class_class)]);
    println!("DynamicLoadObject -> {r:?}");
}
