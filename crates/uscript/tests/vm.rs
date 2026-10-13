//! Interpreter tests that run original compiled script functions from the user's install.
//! Skipped (pass trivially) when the game is not installed.

use std::path::PathBuf;
use uscript::vm::{Value, Vm};

fn system_dir() -> Option<PathBuf> {
    let root = std::env::var("POSTAL2_DIR").unwrap_or_else(|_| r"G:\SteamLibrary\steamapps\common\POSTAL2Complete".into());
    let p = PathBuf::from(root).join("System");
    p.join("Core.u").is_file().then_some(p)
}

macro_rules! vm_or_skip {
    () => {
        match system_dir() {
            Some(d) => Vm::new(d).expect("vm"),
            None => {
                eprintln!("POSTAL 2 install not found; skipping");
                return;
            }
        }
    };
}

#[test]
fn core_randrange_pure_math() {
    let mut vm = vm_or_skip!();
    let object = vm.find_class("Core", "Object").unwrap();
    // Object.RandRange is non-static script code: Min + (Max - Min) * FRand().
    let o = vm.new_object(object);
    for _ in 0..200 {
        let v = vm.call(object, Some(o), "RandRange", vec![Value::Float(10.0), Value::Float(20.0)]).unwrap();
        let Value::Float(x) = v else { panic!("RandRange returned {v:?}") };
        assert!((10.0..20.0).contains(&x), "{x}");
    }
}

#[test]
fn core_rotator_spring_interp() {
    let mut vm = vm_or_skip!();
    let object = vm.find_class("Core", "Object").unwrap();
    // Optional SpringConst omitted: the script substitutes 2048.
    let dest = Value::Rotator([0, 1000, 0]);
    let cur = Value::Rotator([0, 0, 0]);
    let v = vm.call(object, None, "RotatorSpringInterp", vec![dest, cur, Value::Float(0.01)]).unwrap();
    // ScrollDelta=(0,1000,0); Accel=2048*Delta=(0,2048000,0); Vel=Accel*0.01=(0,20480,0); Cur+Vel*0.01=(0,204,0)
    assert_eq!(v, Value::Rotator([0, 204, 0]));
}

#[test]
fn engine_canvas_makecolor_struct_and_optional() {
    let mut vm = vm_or_skip!();
    let canvas = vm.find_class("Engine", "Canvas").unwrap();
    let v = vm.call(canvas, None, "MakeColor", vec![Value::Byte(10), Value::Byte(20), Value::Byte(30)]).unwrap();
    let Value::Struct(fields) = v else { panic!("{v:?}") };
    let get = |n: &str| fields.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(_, v)| v.as_int());
    assert_eq!((get("R"), get("G"), get("B"), get("A")), (Some(10), Some(20), Some(30), Some(255)));
}

#[test]
fn engine_testinfo_statics_defaults_and_asserts() {
    let mut vm = vm_or_skip!();
    let ti = vm.find_class("Engine", "TestInfo").unwrap();
    // TestStatic asserts on its argument and on default.xnum (777 from defaultproperties), then
    // calls another static function whose result it also asserts.
    let v = vm.call(ti, None, "TestStatic", vec![Value::Int(123)]).unwrap();
    assert_eq!(v, Value::Int(123));
    // A wrong argument must trip the script's own assert.
    assert!(vm.call(ti, None, "TestStatic", vec![Value::Int(5)]).is_err());
}

#[test]
fn engine_testinfo_struct_bools_on_instance() {
    let mut vm = vm_or_skip!();
    let ti = vm.find_class("Engine", "TestInfo").unwrap();
    let o = vm.new_object(ti);
    vm.call(ti, Some(o), "TestStructBools", vec![]).unwrap();
    // TestX: MyArray[int(b)] = 0; MyArray[int(b)]++ exercises fixed arrays + out-param natives.
    vm.call(ti, Some(o), "TestX", vec![Value::Bool(true)]).unwrap();
    let Some(Value::Array(a)) = vm.get_prop(o, "MyArray") else { panic!("MyArray missing") };
    assert_eq!(a[1], Value::Int(1));
}

#[test]
fn hello_world_commandlet_logs() {
    let mut vm = vm_or_skip!();
    let c = vm.find_class("Core", "HelloWorldCommandlet").unwrap();
    let o = vm.new_object(c);
    vm.set_prop(o, "intparm", Value::Int(42));
    let r = vm.call(c, Some(o), "Main", vec![Value::Str("-x".into())]).unwrap();
    assert_eq!(r, Value::Int(0));
    assert_eq!(
        vm.log,
        vec![
            "ScriptLog: Hello, world!".to_string(),
            "ScriptLog: Command line parameters=-x".to_string(),
            "ScriptLog: You specified intparm=42".to_string(),
        ]
    );
}

#[test]
fn exec_args_follow_parameter_types() {
    let mut vm = vm_or_skip!();
    let pc = vm.find_class("Postal2Game", "P2Player").unwrap();
    let p = vm.new_object(pc);
    assert_eq!(vm.exec_args(p, "SwitchWeapon", "2"), Some(vec![Value::Byte(2)]));
    let cm = vm.find_class("Engine", "CheatManager").unwrap();
    let c = vm.new_object(cm);
    assert_eq!(vm.exec_args(c, "SetJumpZ", "420.5"), Some(vec![Value::Float(420.5)]));
    assert_eq!(vm.exec_args(c, "Summon", "People.Bystander"), Some(vec![Value::Str("People.Bystander".into())]));
    assert_eq!(vm.exec_args(c, "NoSuchExec", ""), None);
}
