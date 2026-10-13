//! Microbenchmark: raw VM dispatch cost (native call through `call`, and a script function).
use std::time::Instant;
use uscript::vm::{Value, Vm};

fn main() {
    let root = std::env::var("POSTAL2_DIR").unwrap_or_else(|_| r"G:\SteamLibrary\steamapps\common\POSTAL2Complete".into());
    let mut vm = Vm::new(format!(r"{root}\System")).unwrap();
    let object = vm.find_class("Core", "Object").unwrap();
    let n = 1_000_000;
    let t = Instant::now();
    for i in 0..n {
        vm.call(object, None, "Less_IntInt", vec![Value::Int(i), Value::Int(5)]).unwrap();
    }
    let e = t.elapsed().as_secs_f64();
    println!("native via call: {:.1} ns/call", e * 1e9 / n as f64);
    let o = vm.new_object(object);
    let t = Instant::now();
    let m = 200_000;
    for _ in 0..m {
        vm.call(object, Some(o), "RandRange", vec![Value::Float(1.0), Value::Float(2.0)]).unwrap();
    }
    let e = t.elapsed().as_secs_f64();
    println!("script RandRange: {:.1} ns/call, {} statements", e * 1e9 / m as f64, vm.steps_total);
}
