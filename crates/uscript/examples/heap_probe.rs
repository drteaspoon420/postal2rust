//! Is the slowdown after a big world run caused by the heap or by the machine?
//! Runs the dispatch microbenchmark, churns the heap like a level load (millions of small
//! allocations kept alive), and runs the microbenchmark again.
use std::time::Instant;
use uscript::vm::{Value, Vm};

/// mimalloc instead of the system allocator (the Windows heap degrades badly under the
/// interpreter's allocation pattern; see the README performance notes).
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn bench(vm: &mut Vm) -> f64 {
    let object = vm.find_class("Core", "Object").unwrap();
    let t = Instant::now();
    for i in 0..200_000 {
        vm.call(object, None, "Less_IntInt", vec![Value::Int(i), Value::Int(5)]).unwrap();
    }
    t.elapsed().as_secs_f64() * 1e9 / 200_000.0
}

fn main() {
    let root = std::env::var("POSTAL2_DIR").unwrap_or_else(|_| r"G:\SteamLibrary\steamapps\common\POSTAL2Complete".into());
    let mut vm = Vm::new(format!(r"{root}\System")).unwrap();
    println!("before: {:.1} ns", bench(&mut vm));
    let mut keep: Vec<Vec<Value>> = Vec::new();
    for i in 0..2_000_000 {
        keep.push(vec![Value::Str(format!("s{i}")), Value::Int(i)]);
        if i % 3 == 0 {
            keep.swap_remove(i as usize / 7 % keep.len());
        }
    }
    println!("after churn ({} kept): {:.1} ns", keep.len(), bench(&mut vm));
    drop(keep);
    println!("after drop: {:.1} ns", bench(&mut vm));
    // Many interleaved short-lived allocations of mixed sizes (the interpreter's pattern).
    let mut pool: Vec<Vec<u8>> = (0..50_000).map(|i| vec![0u8; 16 + i % 400]).collect();
    for i in 0..20_000_000usize {
        let k = (i * 2654435761) % pool.len();
        pool[k] = vec![0u8; 8 + (i * 31) % 900];
    }
    println!("after mixed churn: {:.1} ns", bench(&mut vm));
    let t = Instant::now();
    let mut x = 0u64;
    for i in 0..300_000_000u64 {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(i);
    }
    println!("pure cpu loop: {:.2}s ({x})", t.elapsed().as_secs_f64());
}
