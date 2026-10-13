//! Native function implementations, registered by declared function name.
//!
//! Semantics follow the documented behaviour of the UnrealScript operators and intrinsics
//! (integer/float arithmetic, string helpers, vector math); unknown natives raise an error
//! naming the missing function so coverage can grow incrementally.

use crate::Result;
use crate::vm::{Ty, Value, Vm};

type A = Vec<Value>;

macro_rules! reg {
    ($vm:expr, $($name:literal => $f:expr),* $(,)?) => {
        $( $vm.register_native($name, $f); )*
    };
}

fn i(a: &A, k: usize) -> i32 {
    a.get(k).map(|v| v.as_int()).unwrap_or(0)
}
fn f(a: &A, k: usize) -> f32 {
    a.get(k).map(|v| v.as_float()).unwrap_or(0.0)
}
fn b(a: &A, k: usize) -> bool {
    a.get(k).map(|v| v.as_bool()).unwrap_or(false)
}
/// Object reference equality: `None` is the same whether it is held as an object, a class, a
/// resource or an unset value (class references compared with `== None` are common).
pub fn obj_eq(a: &Value, b: &Value) -> bool {
    let none = |v: &Value| matches!(v, Value::Object(None) | Value::Class(None) | Value::Void);
    match (a, b) {
        _ if none(a) || none(b) => none(a) && none(b),
        (Value::Object(x), Value::Object(y)) => x == y,
        (Value::Class(x), Value::Class(y)) => x == y,
        (Value::Resource(x), Value::Resource(y)) => x.eq_ignore_ascii_case(y),
        _ => false,
    }
}

fn s(a: &A, k: usize) -> String {
    a.get(k).map(|v| v.as_str()).unwrap_or_default()
}
/// Borrowed string view of a value (no allocation for strings and names).
fn str_of(v: &Value) -> std::borrow::Cow<'_, str> {
    match v {
        Value::Str(s) | Value::Name(s) => std::borrow::Cow::Borrowed(s.as_str()),
        v => std::borrow::Cow::Owned(v.as_str()),
    }
}
fn sref<R>(a: &A, k: usize, f: impl FnOnce(&str) -> R) -> R {
    match a.get(k) {
        Some(v) => f(&str_of(v)),
        None => f(""),
    }
}
fn v(a: &A, k: usize) -> [f32; 3] {
    a.get(k).map(|v| v.as_vec()).unwrap_or([0.0; 3])
}
fn r(a: &A, k: usize) -> [i32; 3] {
    a.get(k).map(|v| v.as_rot()).unwrap_or([0; 3])
}
fn vsize(x: [f32; 3]) -> f32 {
    (x[0] * x[0] + x[1] * x[1] + x[2] * x[2]).sqrt()
}

/// Compound assignment on the first (out) parameter: computes, stores, returns the new value.
fn assign(a: &mut A, v: Value) -> Result<Value> {
    a[0] = v.clone();
    Ok(v)
}

fn chars(x: &str) -> Vec<char> {
    x.chars().collect()
}

fn mid(x: &str, start: i32, count: Option<i32>) -> String {
    let c = chars(x);
    let start = start.max(0) as usize;
    if start >= c.len() {
        return String::new();
    }
    let end = match count {
        Some(n) => (start + n.max(0) as usize).min(c.len()),
        None => c.len(),
    };
    c[start..end].iter().collect()
}

pub fn register_core(vm: &mut Vm) {
    // Integer operators.
    reg!(vm,
        "Add_IntInt" => |_, a| Ok(Value::Int(i(a, 0).wrapping_add(i(a, 1)))),
        "Subtract_IntInt" => |_, a| Ok(Value::Int(i(a, 0).wrapping_sub(i(a, 1)))),
        "Multiply_IntInt" => |_, a| Ok(Value::Int(i(a, 0).wrapping_mul(i(a, 1)))),
        "Divide_IntInt" => |_, a| Ok(Value::Int(if i(a, 1) == 0 { 0 } else { i(a, 0).wrapping_div(i(a, 1)) })),
        "Subtract_PreInt" => |_, a| Ok(Value::Int(i(a, 0).wrapping_neg())),
        "Complement_PreInt" => |_, a| Ok(Value::Int(!i(a, 0))),
        "LessLess_IntInt" => |_, a| Ok(Value::Int(i(a, 0).wrapping_shl(i(a, 1) as u32))),
        "GreaterGreater_IntInt" => |_, a| Ok(Value::Int(i(a, 0).wrapping_shr(i(a, 1) as u32))),
        "GreaterGreaterGreater_IntInt" => |_, a| Ok(Value::Int(((i(a, 0) as u32).wrapping_shr(i(a, 1) as u32)) as i32)),
        "And_IntInt" => |_, a| Ok(Value::Int(i(a, 0) & i(a, 1))),
        "Or_IntInt" => |_, a| Ok(Value::Int(i(a, 0) | i(a, 1))),
        "Xor_IntInt" => |_, a| Ok(Value::Int(i(a, 0) ^ i(a, 1))),
        "Less_IntInt" => |_, a| Ok(Value::Bool(i(a, 0) < i(a, 1))),
        "Greater_IntInt" => |_, a| Ok(Value::Bool(i(a, 0) > i(a, 1))),
        "LessEqual_IntInt" => |_, a| Ok(Value::Bool(i(a, 0) <= i(a, 1))),
        "GreaterEqual_IntInt" => |_, a| Ok(Value::Bool(i(a, 0) >= i(a, 1))),
        "EqualEqual_IntInt" => |_, a| Ok(Value::Bool(i(a, 0) == i(a, 1))),
        "NotEqual_IntInt" => |_, a| Ok(Value::Bool(i(a, 0) != i(a, 1))),
        "AddEqual_IntInt" => |_, a| { let n = i(a, 0).wrapping_add(i(a, 1)); assign(a, Value::Int(n)) },
        "SubtractEqual_IntInt" => |_, a| { let n = i(a, 0).wrapping_sub(i(a, 1)); assign(a, Value::Int(n)) },
        "MultiplyEqual_IntFloat" => |_, a| { let n = (i(a, 0) as f32 * f(a, 1)) as i32; assign(a, Value::Int(n)) },
        "DivideEqual_IntFloat" => |_, a| { let n = (i(a, 0) as f32 / f(a, 1)) as i32; assign(a, Value::Int(n)) },
        "AddAdd_PreInt" => |_, a| { let n = i(a, 0).wrapping_add(1); assign(a, Value::Int(n)) },
        "SubtractSubtract_PreInt" => |_, a| { let n = i(a, 0).wrapping_sub(1); assign(a, Value::Int(n)) },
        "AddAdd_Int" => |_, a| { let o = i(a, 0); a[0] = Value::Int(o.wrapping_add(1)); Ok(Value::Int(o)) },
        "SubtractSubtract_Int" => |_, a| { let o = i(a, 0); a[0] = Value::Int(o.wrapping_sub(1)); Ok(Value::Int(o)) },
        "Rand" => |vm, a| { let m = i(a, 0); Ok(Value::Int(if m > 0 { ((vm.frand() * m as f32) as i32).min(m - 1) } else { 0 })) },
        "Min" => |_, a| Ok(Value::Int(i(a, 0).min(i(a, 1)))),
        "Max" => |_, a| Ok(Value::Int(i(a, 0).max(i(a, 1)))),
        "Clamp" => |_, a| Ok(Value::Int(i(a, 0).max(i(a, 1)).min(i(a, 2)))),
    );
    // Byte operators.
    reg!(vm,
        "AddEqual_ByteByte" => |_, a| { let n = (i(a, 0) as u8).wrapping_add(i(a, 1) as u8); assign(a, Value::Byte(n)) },
        "SubtractEqual_ByteByte" => |_, a| { let n = (i(a, 0) as u8).wrapping_sub(i(a, 1) as u8); assign(a, Value::Byte(n)) },
        "MultiplyEqual_ByteByte" => |_, a| { let n = (i(a, 0) as u8).wrapping_mul(i(a, 1) as u8); assign(a, Value::Byte(n)) },
        "DivideEqual_ByteByte" => |_, a| { let d = i(a, 1) as u8; let n = if d == 0 { 0 } else { i(a, 0) as u8 / d }; assign(a, Value::Byte(n)) },
        "AddAdd_PreByte" => |_, a| { let n = (i(a, 0) as u8).wrapping_add(1); assign(a, Value::Byte(n)) },
        "SubtractSubtract_PreByte" => |_, a| { let n = (i(a, 0) as u8).wrapping_sub(1); assign(a, Value::Byte(n)) },
        "AddAdd_Byte" => |_, a| { let o = i(a, 0) as u8; a[0] = Value::Byte(o.wrapping_add(1)); Ok(Value::Byte(o)) },
        "SubtractSubtract_Byte" => |_, a| { let o = i(a, 0) as u8; a[0] = Value::Byte(o.wrapping_sub(1)); Ok(Value::Byte(o)) },
    );
    // Bool operators (the && / || short-circuit forms are handled by the interpreter).
    reg!(vm,
        "Not_PreBool" => |_, a| Ok(Value::Bool(!b(a, 0))),
        "EqualEqual_BoolBool" => |_, a| Ok(Value::Bool(b(a, 0) == b(a, 1))),
        "NotEqual_BoolBool" => |_, a| Ok(Value::Bool(b(a, 0) != b(a, 1))),
        "AndAnd_BoolBool" => |_, a| Ok(Value::Bool(b(a, 0) && b(a, 1))),
        "OrOr_BoolBool" => |_, a| Ok(Value::Bool(b(a, 0) || b(a, 1))),
        "XorXor_BoolBool" => |_, a| Ok(Value::Bool(b(a, 0) != b(a, 1))),
    );
    // Float operators and math.
    reg!(vm,
        "Add_FloatFloat" => |_, a| Ok(Value::Float(f(a, 0) + f(a, 1))),
        "Subtract_FloatFloat" => |_, a| Ok(Value::Float(f(a, 0) - f(a, 1))),
        "Multiply_FloatFloat" => |_, a| Ok(Value::Float(f(a, 0) * f(a, 1))),
        "Divide_FloatFloat" => |_, a| Ok(Value::Float(f(a, 0) / f(a, 1))),
        "Percent_FloatFloat" => |_, a| Ok(Value::Float(f(a, 0) % f(a, 1))),
        "MultiplyMultiply_FloatFloat" => |_, a| Ok(Value::Float(f(a, 0).powf(f(a, 1)))),
        "Subtract_PreFloat" => |_, a| Ok(Value::Float(-f(a, 0))),
        "Less_FloatFloat" => |_, a| Ok(Value::Bool(f(a, 0) < f(a, 1))),
        "Greater_FloatFloat" => |_, a| Ok(Value::Bool(f(a, 0) > f(a, 1))),
        "LessEqual_FloatFloat" => |_, a| Ok(Value::Bool(f(a, 0) <= f(a, 1))),
        "GreaterEqual_FloatFloat" => |_, a| Ok(Value::Bool(f(a, 0) >= f(a, 1))),
        "EqualEqual_FloatFloat" => |_, a| Ok(Value::Bool(f(a, 0) == f(a, 1))),
        "NotEqual_FloatFloat" => |_, a| Ok(Value::Bool(f(a, 0) != f(a, 1))),
        "ComplementEqual_FloatFloat" => |_, a| Ok(Value::Bool((f(a, 0) - f(a, 1)).abs() < 1e-4)),
        "AddEqual_FloatFloat" => |_, a| { let n = f(a, 0) + f(a, 1); assign(a, Value::Float(n)) },
        "SubtractEqual_FloatFloat" => |_, a| { let n = f(a, 0) - f(a, 1); assign(a, Value::Float(n)) },
        "MultiplyEqual_FloatFloat" => |_, a| { let n = f(a, 0) * f(a, 1); assign(a, Value::Float(n)) },
        "DivideEqual_FloatFloat" => |_, a| { let n = f(a, 0) / f(a, 1); assign(a, Value::Float(n)) },
        "Abs" => |_, a| Ok(Value::Float(f(a, 0).abs())),
        "Sin" => |_, a| Ok(Value::Float(f(a, 0).sin())),
        "Cos" => |_, a| Ok(Value::Float(f(a, 0).cos())),
        "Tan" => |_, a| Ok(Value::Float(f(a, 0).tan())),
        "Asin" => |_, a| Ok(Value::Float(f(a, 0).asin())),
        "Acos" => |_, a| Ok(Value::Float(f(a, 0).acos())),
        "Atan" => |_, a| Ok(Value::Float(f(a, 0).atan())),
        "Atan2" => |_, a| Ok(Value::Float(f(a, 0).atan2(f(a, 1)))),
        "Exp" => |_, a| Ok(Value::Float(f(a, 0).exp())),
        "Loge" => |_, a| Ok(Value::Float(f(a, 0).ln())),
        "Log10" => |_, a| Ok(Value::Float(f(a, 0).log10())),
        "Sqrt" => |_, a| Ok(Value::Float(f(a, 0).sqrt())),
        "Square" => |_, a| Ok(Value::Float(f(a, 0) * f(a, 0))),
        "Ceil" => |_, a| Ok(Value::Float(f(a, 0).ceil())),
        "Floor" => |_, a| Ok(Value::Float(f(a, 0).floor())),
        "FRand" => |vm, _| Ok(Value::Float(vm.frand())),
        "FMin" => |_, a| Ok(Value::Float(f(a, 0).min(f(a, 1)))),
        "FMax" => |_, a| Ok(Value::Float(f(a, 0).max(f(a, 1)))),
        "FClamp" => |_, a| Ok(Value::Float(f(a, 0).max(f(a, 1)).min(f(a, 2)))),
        "Lerp" => |_, a| Ok(Value::Float(f(a, 1) + (f(a, 2) - f(a, 1)) * f(a, 0))),
        "Smerp" => |_, a| { let t = f(a, 0); Ok(Value::Float(f(a, 1) + (f(a, 2) - f(a, 1)) * (3.0 * t * t - 2.0 * t * t * t))) },
    );
    // Strings.
    reg!(vm,
        "Concat_StrStr" => |_, a| Ok(Value::Str(s(a, 0) + &s(a, 1))),
        "At_StrStr" => |_, a| Ok(Value::Str(format!("{} {}", s(a, 0), s(a, 1)))),
        "Less_StrStr" => |_, a| Ok(Value::Bool(s(a, 0) < s(a, 1))),
        "Greater_StrStr" => |_, a| Ok(Value::Bool(s(a, 0) > s(a, 1))),
        "LessEqual_StrStr" => |_, a| Ok(Value::Bool(s(a, 0) <= s(a, 1))),
        "GreaterEqual_StrStr" => |_, a| Ok(Value::Bool(s(a, 0) >= s(a, 1))),
        "EqualEqual_StrStr" => |_, a| Ok(Value::Bool(sref(a, 0, |x| sref(a, 1, |y| x == y)))),
        "NotEqual_StrStr" => |_, a| Ok(Value::Bool(sref(a, 0, |x| sref(a, 1, |y| x != y)))),
        "ComplementEqual_StrStr" => |_, a| Ok(Value::Bool(sref(a, 0, |x| sref(a, 1, |y| x.eq_ignore_ascii_case(y))))),
        "Len" => |_, a| Ok(Value::Int(sref(a, 0, |x| x.chars().count() as i32))),
        "InStr" => |_, a| {
            // Character index of the first match (byte search, then count chars before it).
            let h = a.first().map(str_of).unwrap_or(std::borrow::Cow::Borrowed(""));
            let n = a.get(1).map(str_of).unwrap_or(std::borrow::Cow::Borrowed(""));
            Ok(Value::Int(match h.find(n.as_ref()) {
                Some(b) => if h.is_ascii() { b as i32 } else { h[..b].chars().count() as i32 },
                None => -1,
            }))
        },
        "Mid" => |_, a| {
            let count = match a.get(2) { Some(Value::Int(0)) | None | Some(Value::Void) => None, _ => Some(i(a, 2)) };
            Ok(Value::Str(mid(&s(a, 0), i(a, 1), count)))
        },
        "Left" => |_, a| Ok(Value::Str(mid(&s(a, 0), 0, Some(i(a, 1))))),
        "Right" => |_, a| { let c = s(a, 0).chars().count() as i32; Ok(Value::Str(mid(&s(a, 0), (c - i(a, 1)).max(0), None))) },
        "Caps" => |_, a| Ok(Value::Str(sref(a, 0, |x| if x.is_ascii() { x.to_ascii_uppercase() } else { x.to_uppercase() }))),
        "Chr" => |_, a| Ok(Value::Str(char::from_u32(i(a, 0) as u32).map(String::from).unwrap_or_default())),
        "Asc" => |_, a| Ok(Value::Int(s(a, 0).chars().next().map(|c| c as i32).unwrap_or(0))),
        "EqualEqual_NameName" => |_, a| Ok(Value::Bool(sref(a, 0, |x| sref(a, 1, |y| x.eq_ignore_ascii_case(y))))),
        "NotEqual_NameName" => |_, a| Ok(Value::Bool(!sref(a, 0, |x| sref(a, 1, |y| x.eq_ignore_ascii_case(y))))),
        "EqualEqual_ObjectObject" => |_, a| Ok(Value::Bool(obj_eq(&a[0], &a[1]))),
        "NotEqual_ObjectObject" => |_, a| Ok(Value::Bool(!obj_eq(&a[0], &a[1]))),
        "Log" => |vm, a| { let tag = match a.get(1) { Some(Value::Name(n)) if !n.eq_ignore_ascii_case("None") => n.clone(), _ => "ScriptLog".into() }; vm.log.push(format!("{tag}: {}", s(a, 0))); Ok(Value::Void) },
        "Warn" => |vm, a| { vm.log.push(format!("ScriptWarning: {}", s(a, 0))); Ok(Value::Void) },
    );
    // Vectors and rotators.
    reg!(vm,
        "Subtract_PreVector" => |_, a| { let x = v(a, 0); Ok(Value::Vector([-x[0], -x[1], -x[2]])) },
        "Add_VectorVector" => |_, a| { let (x, y) = (v(a, 0), v(a, 1)); Ok(Value::Vector([x[0] + y[0], x[1] + y[1], x[2] + y[2]])) },
        "Subtract_VectorVector" => |_, a| { let (x, y) = (v(a, 0), v(a, 1)); Ok(Value::Vector([x[0] - y[0], x[1] - y[1], x[2] - y[2]])) },
        "Multiply_VectorFloat" => |_, a| { let (x, k) = (v(a, 0), f(a, 1)); Ok(Value::Vector([x[0] * k, x[1] * k, x[2] * k])) },
        "Multiply_FloatVector" => |_, a| { let (k, x) = (f(a, 0), v(a, 1)); Ok(Value::Vector([x[0] * k, x[1] * k, x[2] * k])) },
        "Multiply_VectorVector" => |_, a| { let (x, y) = (v(a, 0), v(a, 1)); Ok(Value::Vector([x[0] * y[0], x[1] * y[1], x[2] * y[2]])) },
        "Divide_VectorFloat" => |_, a| { let (x, k) = (v(a, 0), f(a, 1)); Ok(Value::Vector([x[0] / k, x[1] / k, x[2] / k])) },
        "Dot_VectorVector" => |_, a| { let (x, y) = (v(a, 0), v(a, 1)); Ok(Value::Float(x[0] * y[0] + x[1] * y[1] + x[2] * y[2])) },
        "Cross_VectorVector" => |_, a| { let (x, y) = (v(a, 0), v(a, 1)); Ok(Value::Vector([x[1] * y[2] - x[2] * y[1], x[2] * y[0] - x[0] * y[2], x[0] * y[1] - x[1] * y[0]])) },
        "EqualEqual_VectorVector" => |_, a| Ok(Value::Bool(v(a, 0) == v(a, 1))),
        "NotEqual_VectorVector" => |_, a| Ok(Value::Bool(v(a, 0) != v(a, 1))),
        "AddEqual_VectorVector" => |_, a| { let (x, y) = (v(a, 0), v(a, 1)); assign(a, Value::Vector([x[0] + y[0], x[1] + y[1], x[2] + y[2]])) },
        "SubtractEqual_VectorVector" => |_, a| { let (x, y) = (v(a, 0), v(a, 1)); assign(a, Value::Vector([x[0] - y[0], x[1] - y[1], x[2] - y[2]])) },
        "MultiplyEqual_VectorFloat" => |_, a| { let (x, k) = (v(a, 0), f(a, 1)); assign(a, Value::Vector([x[0] * k, x[1] * k, x[2] * k])) },
        "DivideEqual_VectorFloat" => |_, a| { let (x, k) = (v(a, 0), f(a, 1)); assign(a, Value::Vector([x[0] / k, x[1] / k, x[2] / k])) },
        "VSize" => |_, a| Ok(Value::Float(vsize(v(a, 0)))),
        "Normal" => |_, a| { let x = v(a, 0); let l = vsize(x); Ok(Value::Vector(if l > 0.0 { [x[0] / l, x[1] / l, x[2] / l] } else { [0.0; 3] })) },
        "Vect" => |_, a| Ok(Value::Vector([f(a, 0), f(a, 1), f(a, 2)])),
        "Rot" => |_, a| Ok(Value::Rotator([i(a, 0), i(a, 1), i(a, 2)])),
        "EqualEqual_RotatorRotator" => |_, a| Ok(Value::Bool(r(a, 0) == r(a, 1))),
        "NotEqual_RotatorRotator" => |_, a| Ok(Value::Bool(r(a, 0) != r(a, 1))),
        "Add_RotatorRotator" => |_, a| { let (x, y) = (r(a, 0), r(a, 1)); Ok(Value::Rotator([x[0].wrapping_add(y[0]), x[1].wrapping_add(y[1]), x[2].wrapping_add(y[2])])) },
        "Subtract_RotatorRotator" => |_, a| { let (x, y) = (r(a, 0), r(a, 1)); Ok(Value::Rotator([x[0].wrapping_sub(y[0]), x[1].wrapping_sub(y[1]), x[2].wrapping_sub(y[2])])) },
        "Multiply_RotatorFloat" => |_, a| { let (x, k) = (r(a, 0), f(a, 1)); Ok(Value::Rotator([(x[0] as f32 * k) as i32, (x[1] as f32 * k) as i32, (x[2] as f32 * k) as i32])) },
        "Multiply_FloatRotator" => |_, a| { let (k, x) = (f(a, 0), r(a, 1)); Ok(Value::Rotator([(x[0] as f32 * k) as i32, (x[1] as f32 * k) as i32, (x[2] as f32 * k) as i32])) },
        "Divide_RotatorFloat" => |_, a| { let (x, k) = (r(a, 0), f(a, 1)); Ok(Value::Rotator([(x[0] as f32 / k) as i32, (x[1] as f32 / k) as i32, (x[2] as f32 / k) as i32])) },
    );
    register_core_more(vm);
}

/// Unreal angle units to radians (65536 units per turn).
pub fn urad(u: i32) -> f32 {
    u as f32 * std::f32::consts::PI / 32768.0
}

fn uang(rad: f32) -> i32 {
    (rad * 32768.0 / std::f32::consts::PI).round() as i32
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn scale(a: [f32; 3], k: f32) -> [f32; 3] {
    [a[0] * k, a[1] * k, a[2] * k]
}

/// The X (forward), Y (right) and Z (up) axes of a rotation (pitch, yaw, roll).
pub fn rot_axes(rot: [i32; 3]) -> [[f32; 3]; 3] {
    let (sp, cp) = urad(rot[0]).sin_cos();
    let (sy, cy) = urad(rot[1]).sin_cos();
    let (sr, cr) = urad(rot[2]).sin_cos();
    [
        [cp * cy, cp * sy, sp],
        [sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, -sr * cp],
        [-(cr * sp * cy + sr * sy), cy * sr - cr * sp * sy, cr * cp],
    ]
}

/// Rotation whose axes are X, Y, Z (inverse of [`rot_axes`]).
pub fn axes_rot(x: [f32; 3], y: [f32; 3], _z: [f32; 3]) -> [i32; 3] {
    let pitch = x[2].atan2((x[0] * x[0] + x[1] * x[1]).sqrt());
    let yaw = x[1].atan2(x[0]);
    let (sp, cp) = pitch.sin_cos();
    let (sy, cy) = yaw.sin_cos();
    let y0 = [-sy, cy, 0.0];
    let w = [sp * cy, sp * sy, -cp];
    let roll = dot(y, w).atan2(dot(y, y0));
    [uang(pitch), uang(yaw), uang(roll)]
}

fn norm_angle(a: i32) -> i32 {
    let r = a & 0xFFFF;
    if r > 32767 { r - 65536 } else { r }
}

/// Splits on a delimiter like the engine: empty delimiter yields the whole string.
fn split_all(s: &str, d: &str) -> Vec<String> {
    if d.is_empty() || s.is_empty() {
        return if s.is_empty() { Vec::new() } else { vec![s.to_string()] };
    }
    s.split(d).map(str::to_string).collect()
}

fn color_get(v: &Value) -> [(String, i32); 4] {
    let mut out: [(String, i32); 4] = [("B".into(), 0), ("G".into(), 0), ("R".into(), 0), ("A".into(), 0)];
    if let Value::Struct(f) = v {
        for (k, (n, x)) in f.iter().enumerate().take(4) {
            out[k] = (n.clone(), x.as_int());
        }
    }
    out
}

fn color_make(c: [(String, i32); 4]) -> Value {
    Value::Struct(c.into_iter().map(|(n, x)| (n, Value::Byte(x.clamp(0, 255) as u8))).collect())
}

fn color_op(a: &Value, b: &Value, op: impl Fn(i32, i32) -> i32) -> Value {
    let (x, y) = (color_get(a), color_get(b));
    let mut out = x.clone();
    for k in 0..4 {
        out[k].1 = op(x[k].1, y[k].1);
    }
    color_make(out)
}

fn color_scale(c: &Value, k: f32) -> Value {
    let mut x = color_get(c);
    for e in x.iter_mut() {
        e.1 = (e.1 as f32 * k) as i32;
    }
    color_make(x)
}

fn spring(dest: f32, cur: f32, dt: f32, k: f32) -> f32 {
    let k = if k == 0.0 { 2048.0 } else { k };
    cur + (dest - cur) * k * dt * dt
}

/// Looks up a property value as text (`GetPropertyText`).
fn prop_text(vm: &Vm, o: usize, name: &str) -> String {
    vm.get_prop(o, name).map(|v| vm.value_string(v)).unwrap_or_default()
}

fn register_core_more(vm: &mut Vm) {
    // Vector / rotator extras.
    reg!(vm,
        "MultiplyEqual_VectorVector" => |_, a| { let (x, y) = (v(a, 0), v(a, 1)); assign(a, Value::Vector([x[0] * y[0], x[1] * y[1], x[2] * y[2]])) },
        "AddEqual_RotatorRotator" => |_, a| { let (x, y) = (r(a, 0), r(a, 1)); assign(a, Value::Rotator([x[0].wrapping_add(y[0]), x[1].wrapping_add(y[1]), x[2].wrapping_add(y[2])])) },
        "SubtractEqual_RotatorRotator" => |_, a| { let (x, y) = (r(a, 0), r(a, 1)); assign(a, Value::Rotator([x[0].wrapping_sub(y[0]), x[1].wrapping_sub(y[1]), x[2].wrapping_sub(y[2])])) },
        "MultiplyEqual_RotatorFloat" => |_, a| { let (x, k) = (r(a, 0), f(a, 1)); assign(a, Value::Rotator([(x[0] as f32 * k) as i32, (x[1] as f32 * k) as i32, (x[2] as f32 * k) as i32])) },
        "DivideEqual_RotatorFloat" => |_, a| { let (x, k) = (r(a, 0), f(a, 1)); assign(a, Value::Rotator([(x[0] as f32 / k) as i32, (x[1] as f32 / k) as i32, (x[2] as f32 / k) as i32])) },
        "GreaterGreater_VectorRotator" => |_, a| {
            let (x, ax) = (v(a, 0), rot_axes(r(a, 1)));
            Ok(Value::Vector(add(add(scale(ax[0], x[0]), scale(ax[1], x[1])), scale(ax[2], x[2]))))
        },
        "LessLess_VectorRotator" => |_, a| {
            let (x, ax) = (v(a, 0), rot_axes(r(a, 1)));
            Ok(Value::Vector([dot(x, ax[0]), dot(x, ax[1]), dot(x, ax[2])]))
        },
        "GetAxes" => |_, a| {
            let ax = rot_axes(r(a, 0));
            for k in 0..3 { a[k + 1] = Value::Vector(ax[k]); }
            Ok(Value::Void)
        },
        "GetUnAxes" => |_, a| {
            let ax = rot_axes(r(a, 0));
            for k in 0..3 { a[k + 1] = Value::Vector([ax[0][k], ax[1][k], ax[2][k]]); }
            Ok(Value::Void)
        },
        "Invert" => |_, a| {
            let m = [v(a, 0), v(a, 1), v(a, 2)];
            for k in 0..3 { a[k] = Value::Vector([m[0][k], m[1][k], m[2][k]]); }
            Ok(Value::Void)
        },
        "OrthoRotation" => |_, a| Ok(Value::Rotator(axes_rot(v(a, 0), v(a, 1), v(a, 2)))),
        "Normalize" => |_, a| { let x = r(a, 0); Ok(Value::Rotator([norm_angle(x[0]), norm_angle(x[1]), norm_angle(x[2])])) },
        "RotRand" => |vm, a| {
            let roll = if b(a, 0) { (vm.frand() * 65536.0) as i32 } else { 0 };
            Ok(Value::Rotator([(vm.frand() * 65536.0) as i32, (vm.frand() * 65536.0) as i32, roll]))
        },
        "VRand" => |vm, _| {
            loop {
                let p = [vm.frand() * 2.0 - 1.0, vm.frand() * 2.0 - 1.0, vm.frand() * 2.0 - 1.0];
                let l = vsize(p);
                if l > 1e-3 && l <= 1.0 {
                    return Ok(Value::Vector(scale(p, 1.0 / l)));
                }
            }
        },
        "MirrorVectorByNormal" => |_, a| {
            let (x, n) = (v(a, 0), v(a, 1));
            let d = 2.0 * dot(x, n);
            Ok(Value::Vector([x[0] - d * n[0], x[1] - d * n[1], x[2] - d * n[2]]))
        },
        "VLerp" => |_, a| { let (x, y, t) = (v(a, 0), v(a, 1), f(a, 2)); Ok(Value::Vector([x[0] + (y[0] - x[0]) * t, x[1] + (y[1] - x[1]) * t, x[2] + (y[2] - x[2]) * t])) },
        "RLerp" => |_, a| {
            let (x, y, t) = (r(a, 0), r(a, 1), f(a, 2));
            let l = |p: i32, q: i32| p + (norm_angle(q - p) as f32 * t) as i32;
            Ok(Value::Rotator([l(x[0], y[0]), l(x[1], y[1]), l(x[2], y[2])]))
        },
        "ClockwiseFrom_IntInt" => |_, a| Ok(Value::Bool(norm_angle(i(a, 0) - i(a, 1)) >= 0)),
        "FloatSpringInterp" => |_, a| Ok(Value::Float(spring(f(a, 0), f(a, 1), f(a, 2), f(a, 3)))),
        "VectorSpringInterp" => |_, a| {
            let (d, c, dt, k) = (v(a, 0), v(a, 1), f(a, 2), f(a, 3));
            Ok(Value::Vector([spring(d[0], c[0], dt, k), spring(d[1], c[1], dt, k), spring(d[2], c[2], dt, k)]))
        },
        "InterpCurveEval" => |_, a| {
            // Piecewise linear through the points (sorted by InVal), clamped at the ends.
            let pts: Vec<(f32, f32)> = match a.first() {
                Some(Value::Struct(f)) => match f.first().map(|x| &x.1) {
                    Some(Value::Array(p)) => p.iter().map(|e| match e {
                        Value::Struct(kv) => (kv.first().map(|x| x.1.as_float()).unwrap_or(0.0), kv.get(1).map(|x| x.1.as_float()).unwrap_or(0.0)),
                        _ => (0.0, 0.0),
                    }).collect(),
                    _ => Vec::new(),
                },
                _ => Vec::new(),
            };
            let x = f(a, 1);
            let Some(first) = pts.first() else { return Ok(Value::Float(0.0)) };
            if x <= first.0 { return Ok(Value::Float(first.1)); }
            for w in pts.windows(2) {
                if x < w[1].0 {
                    let t = if w[1].0 > w[0].0 { (x - w[0].0) / (w[1].0 - w[0].0) } else { 0.0 };
                    return Ok(Value::Float(w[0].1 + (w[1].1 - w[0].1) * t));
                }
            }
            Ok(Value::Float(pts.last().unwrap().1))
        },
    );
    // Strings.
    reg!(vm,
        "SplitString" => |_, a| {
            let parts = split_all(&s(a, 0), &s(a, 1));
            let n = parts.len() as i32;
            let mut list = match a.get(2) { Some(Value::Array(l)) => l.clone(), _ => Vec::new() };
            list.extend(parts.into_iter().map(Value::Str));
            a[2] = Value::Array(list);
            Ok(Value::Int(n))
        },
        "SplitStringSingle" => |_, a| {
            let (src, d) = (s(a, 0), s(a, 1));
            match (!d.is_empty()).then(|| src.find(&d)).flatten() {
                Some(p) => {
                    a[2] = Value::Str(src[..p].to_string());
                    a[0] = Value::Str(src[p + d.len()..].to_string());
                    Ok(Value::Bool(true))
                }
                None => {
                    let found = !src.is_empty();
                    a[2] = Value::Str(src);
                    a[0] = Value::Str(String::new());
                    Ok(Value::Bool(found))
                }
            }
        },
        "Localize" => |vm, a| { let (sec, key, pkg) = (s(a, 0), s(a, 1), s(a, 2)); Ok(Value::Str(vm.localize(&sec, &key, &pkg))) },
    );
    // Object / class / state natives.
    reg!(vm,
        "GotoState" => |vm, a| {
            let Some(o) = vm.native_self() else { return Ok(Value::Void) };
            // An omitted state name means the current state (UE2 execGotoState defaults it to
            // the current state's name): Mover.MakeGroupReturn's `GotoState( , 'Close')` must stay
            // in the door's state; as '' it dropped the door into no state, Bump stayed disabled
            // and the door never answered the player again.
            let st = match a.first() {
                None | Some(Value::Void) => vm.state_name(o),
                Some(v) => v.as_name(),
            };
            let label = a.get(1).map(|x| x.as_name()).unwrap_or_default();
            vm.goto_state(o, &st, &label)?;
            Ok(Value::Void)
        },
        "IsInState" => |vm, a| Ok(Value::Bool(vm.native_self().is_some_and(|o| vm.is_in_state(o, &a[0].as_name())))),
        "GetStateName" => |vm, _| Ok(Value::Name(vm.native_self().map(|o| vm.state_name(o)).unwrap_or_else(|| "None".into()))),
        "Enable" => |vm, a| { if let Some(o) = vm.native_self() { vm.set_probe_enabled(o, &a[0].as_name(), true); } Ok(Value::Void) },
        "Disable" => |vm, a| { if let Some(o) = vm.native_self() { vm.set_probe_enabled(o, &a[0].as_name(), false); } Ok(Value::Void) },
        "IsA" => |vm, a| Ok(Value::Bool(vm.native_self().is_some_and(|o| vm.class_is_a(vm.objects[o].class, &a[0].as_name())))),
        "ClassIsChildOf" => |vm, a| Ok(Value::Bool(match (a[0].as_class(), a[1].as_class()) {
            (Some(x), Some(y)) => vm.is_child_of(x, y),
            _ => false,
        })),
        "GetEnum" => |vm, a| {
            let names = match &a[0] { Value::Resource(p) => vm.enum_names(p), _ => None };
            Ok(Value::Name(names.and_then(|n| n.get(i(a, 1).max(0) as usize).cloned()).unwrap_or_else(|| "None".into())))
        },
        "DynamicLoadObject" => |vm, a| load_object(vm, a, true),
        "FindObject" => |vm, a| load_object(vm, a, false),
        "GetPropertyText" => |vm, a| Ok(Value::Str(vm.native_self().map(|o| prop_text(vm, o, &s(a, 0))).unwrap_or_default())),
        "SetPropertyText" => |vm, a| {
            let Some(o) = vm.native_self() else { return Ok(Value::Void) };
            let (name, text) = (s(a, 0), s(a, 1));
            let c = vm.objects[o].class;
            if let Some(slot) = vm.classes[c].slot(&name) {
                let v = match &vm.classes[c].slots[slot].ty {
                    Ty::Int => Value::Int(crate::vm::parse_int(&text)),
                    Ty::Byte => Value::Byte(crate::vm::parse_int(&text) as u8),
                    Ty::Float => Value::Float(text.trim().parse().unwrap_or(0.0)),
                    Ty::Bool => Value::Bool(text.trim().eq_ignore_ascii_case("true") || text.trim() == "1"),
                    Ty::Name => Value::Name(text),
                    Ty::Str => Value::Str(text),
                    Ty::Class => vm.find_class_by_name(&text).ok().map(|c| Value::Class(Some(c))).unwrap_or(Value::Class(None)),
                    _ => return Ok(Value::Void),
                };
                vm.objects[o].slots[slot] = v;
            }
            Ok(Value::Void)
        },
        // SaveConfig / StaticSaveConfig / ResetConfig live in crate::config.
        "PlatformIsWindows" => |_, _| Ok(Value::Bool(cfg!(windows))),
        "PlatformIsUnix" => |_, _| Ok(Value::Bool(cfg!(all(unix, not(target_os = "macos"))))),
        "PlatformIsMacOS" => |_, _| Ok(Value::Bool(cfg!(target_os = "macos"))),
        "PlatformIsSteamDeck" => |_, _| Ok(Value::Bool(false)),
        "GetBuildDate" => |_, _| Ok(Value::Str(concat!("postal2rust ", env!("CARGO_PKG_VERSION")).into())),
        "GetCountryCode" => |_, _| Ok(Value::Str("int".into())),
        // File access is read-only and relative to the System directory.
        "FileSize" => |vm, a| Ok(Value::Int(std::fs::metadata(vm.system_dir.join(s(a, 0))).map(|m| m.len().min(i32::MAX as u64) as i32).unwrap_or(-1))),
        "LoadFileToString" => |vm, a| match std::fs::read(vm.system_dir.join(s(a, 1))) {
            Ok(b) => { a[0] = Value::Str(String::from_utf8_lossy(&b).into_owned()); Ok(Value::Bool(true)) }
            Err(_) => Ok(Value::Bool(false)),
        },
        "SaveStringToFile" => |_, _| Ok(Value::Bool(false)),
        "FindFiles" => |vm, a| {
            let pat = s(a, 0);
            let (want_files, want_dirs) = (b(a, 1), b(a, 2));
            let path = vm.system_dir.join(&pat);
            let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
            let mask = path.file_name().map(|m| m.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
            let mut out = Vec::new();
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for e in rd.flatten() {
                    let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                    let n = e.file_name().to_string_lossy().into_owned();
                    if (is_dir && want_dirs || !is_dir && want_files) && wildcard(&mask, &n.to_ascii_lowercase()) {
                        out.push(Value::Str(n));
                    }
                }
            }
            Ok(Value::Array(out))
        },
    );
}

/// Color operators (declared on Actor).
pub fn register_color(vm: &mut Vm) {
    reg!(vm,
        "Add_ColorColor" => |_, a| Ok(color_op(&a[0], &a[1], |x, y| x + y)),
        "Subtract_ColorColor" => |_, a| Ok(color_op(&a[0], &a[1], |x, y| x - y)),
        "Multiply_ColorFloat" => |_, a| Ok(color_scale(&a[0], f(a, 1))),
        "Multiply_FloatColor" => |_, a| Ok(color_scale(&a[1], f(a, 0))),
    );
}

/// `*` / `?` wildcard match.
fn wildcard(p: &str, s: &str) -> bool {
    let (p, s): (Vec<char>, Vec<char>) = (p.chars().collect(), s.chars().collect());
    fn m(p: &[char], s: &[char]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some('*') => m(&p[1..], s) || (!s.is_empty() && m(p, &s[1..])),
            Some('?') => !s.is_empty() && m(&p[1..], &s[1..]),
            Some(c) => s.first() == Some(c) && m(&p[1..], &s[1..]),
        }
    }
    m(&p, &s)
}

/// `DynamicLoadObject` / `FindObject`: script classes become `Class` values (loading the
/// package when `load`), other objects are returned as resource references by path.
fn load_object(vm: &mut Vm, a: &mut A, load: bool) -> Result<Value> {
    let name = s(a, 0);
    if name.is_empty() {
        return Ok(Value::Object(None));
    }
    // `class'Class'` (or no class) asks for a script class. `Class(None)` is a native-only class
    // such as `Font` or `Texture` that has no script definition: try a class, else a resource.
    let want_class = match a.get(1) {
        Some(Value::Class(Some(c))) => vm.classes[*c].name.eq_ignore_ascii_case("Class"),
        Some(Value::Class(None)) => false,
        // `class'Class'` itself may arrive as a resource reference (`Core.Class`).
        Some(Value::Resource(p)) => p.rsplit('.').next().is_some_and(|n| n.eq_ignore_ascii_case("Class")),
        _ => true,
    };
    if want_class || matches!(a.get(1), Some(Value::Class(None)) | None) {
        if let Some((pkg, cls)) = name.split_once('.') {
            let loaded = load || (0..vm.pkgs.len()).any(|p| vm.pkgs[p].pkg.name.eq_ignore_ascii_case(pkg));
            if loaded && !cls.contains('.') {
                if let Ok(c) = vm.find_class(pkg, cls) {
                    return Ok(Value::Class(Some(c)));
                }
            }
        } else if want_class {
            // A bare class name (`iwant KrotchyInv`): the engine finds it in any loaded package.
            let hit = (0..vm.pkgs.len()).find_map(|p| vm.pkgs[p].find_class(&name).map(|i| (p, i)));
            if let Some((p, i)) = hit {
                if let Ok(c) = vm.load_class(crate::vm::FieldId { pkg: p as u32, export: i as u32 }) {
                    return Ok(Value::Class(Some(c)));
                }
            }
        }
        if want_class {
            return Ok(Value::Class(None));
        }
    }
    // Objects of script classes (MovieTexture, ...) can be backed by the host.
    if load {
        if let Some(Value::Class(Some(c))) = a.get(1) {
            let c = *c;
            let h = vm.host();
            if let Some(v) = h.load_object(vm, &name, c) {
                return Ok(v);
            }
        }
    }
    Ok(if load { Value::Resource(name) } else { Value::Object(None) })
}
