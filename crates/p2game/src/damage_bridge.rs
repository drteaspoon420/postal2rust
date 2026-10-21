//! Routes a registered external player through the existing TakeDamage watch.
//! Ordinary Postal actors keep their script damage path. The bridge consumes
//! each incoming hit before a subclass can forward it to Super.TakeDamage.
use std::cell::RefCell;
use uscript::vm::{ClassId, ObjId, Value};

#[derive(Clone, Debug)]
pub struct IncomingDamage {
    pub amount: f32,
    pub instigator: Option<ObjId>,
    pub location: [f32; 3],
    pub momentum: [f32; 3],
    pub damage_type: Option<ClassId>,
}
#[derive(Default)]
struct Route {
    player: Option<ObjId>,
    hits: Vec<IncomingDamage>,
}
thread_local! { static ROUTE: RefCell<Route> = RefCell::new(Route::default()); }

/// Physics installs its call watches on each level; stale actor ids must not
/// survive that boundary, including when the next level reuses an object id.
pub fn reset() {
    ROUTE.with(|r| *r.borrow_mut() = Route::default());
}
pub fn register(player: ObjId) {
    ROUTE.with(|r| {
        let mut r = r.borrow_mut();
        if r.player != Some(player) {
            r.hits.clear();
            r.player = Some(player);
        }
    });
}
pub fn take(player: ObjId) -> Vec<IncomingDamage> {
    ROUTE.with(|r| {
        let mut r = r.borrow_mut();
        if r.player == Some(player) {
            std::mem::take(&mut r.hits)
        } else {
            Vec::new()
        }
    })
}
/// Returns true when the external owner accepted this hit. Zeroing the
/// arguments also makes nested Super calls harmless without suppressing other
/// victims' damage, the script call stack, or normal Postal hit bookkeeping.
pub fn intercept(player: ObjId, args: &mut [Value]) -> bool {
    ROUTE.with(|r| {
        let mut r = r.borrow_mut();
        if r.player != Some(player) {
            return false;
        }
        let amount = args.first().map(Value::as_float).unwrap_or(0.);
        if !amount.is_finite() || amount <= 0. {
            return false;
        }
        r.hits.push(IncomingDamage {
            amount,
            instigator: args.get(1).and_then(Value::as_obj),
            location: args.get(2).map(Value::as_vec).unwrap_or_default(),
            momentum: args.get(3).map(Value::as_vec).unwrap_or_default(),
            damage_type: match args.get(4) {
                Some(Value::Class(c)) => *c,
                _ => None,
            },
        });
        args[0] = Value::Int(0);
        if let Some(momentum) = args.get_mut(3) {
            *momentum = Value::Vector([0.; 3]);
        }
        true
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn player_damage_is_consumed_once_and_other_pawns_keep_their_arguments() {
        reset();
        register(7);
        let args = vec![
            Value::Int(25),
            Value::Object(Some(9)),
            Value::Vector([1., 2., 3.]),
            Value::Vector([4., 5., 6.]),
            Value::Class(None),
        ];
        let mut other = args.clone();
        assert!(!intercept(8, &mut other));
        assert_eq!(other[0].as_int(), 25);
        let mut player = args;
        assert!(intercept(7, &mut player));
        assert_eq!(player[0].as_int(), 0);
        assert_eq!(player[3].as_vec(), [0.; 3]);
        assert!(
            !intercept(7, &mut player),
            "Super.TakeDamage must not queue a second hit"
        );
        let hits = take(7);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].amount, 25.);
        assert_eq!(hits[0].instigator, Some(9));
        assert_eq!(hits[0].momentum, [4., 5., 6.]);
        assert!(take(7).is_empty());
        register(12);
        assert!(!intercept(7, &mut [Value::Int(30)]));
        reset();
        assert!(!intercept(12, &mut [Value::Int(30)]));
    }
}
