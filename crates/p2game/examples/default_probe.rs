//! Research probe: prints class default properties through the VM.
//! Usage: default_probe Package.Class Prop [Prop...]
use uscript::vm::{Value, Vm};

/// Value text with array elements and struct fields expanded.
fn show(vm: &Vm, v: &Value) -> String {
    match v {
        Value::Array(a) => format!("[{}]", a.iter().map(|e| show(vm, e)).collect::<Vec<_>>().join(", ")),
        Value::Struct(f) => format!("{{{}}}", f.iter().map(|(k, e)| format!("{k}={}", show(vm, e))).collect::<Vec<_>>().join(", ")),
        Value::Class(Some(c)) => vm.classes[*c].name.clone(),
        v => vm.value_string(v),
    }
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let root = p2game::game_root(None);
    let mut vm = Vm::new(root.join("System")).unwrap();
    let name = a.first().cloned().unwrap_or_else(|| "FX.SizeableWoof".into());
    let (p, c) = name.split_once('.').unwrap();
    for prop in &a[1..] {
        match vm.class_default(p, c, prop) {
            Ok(Some(v)) => println!("{name}.{prop} = {}", show(&vm, &v)),
            Ok(None) => println!("{name}.{prop}: no such property"),
            Err(e) => println!("{name}.{prop}: {e}"),
        }
    }
}
