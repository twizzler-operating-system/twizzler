#![feature(fn_traits)]
#![feature(unboxed_closures)]
#![feature(tuple_trait)]

use std::marker::{PhantomData, Tuple};

pub use secgate_macros::*;

pub struct SecurityGate<Imp, Args, Ret> {
    imp: Imp,
    _pd: PhantomData<(Imp, Args, Ret)>,
}

fn trampoline<Imp, Args: Tuple, Ret>(
    _sg: &SecurityGate<Imp, Args, Ret>,
    args: Args,
    imp: &Imp,
) -> Ret
where
    Imp: Fn<Args, Output = Ret>,
{
    // TODO: any pre-call setup
    imp.call(args)
}

impl<F, A: Tuple, R> FnOnce<A> for SecurityGate<F, A, R>
where
    F: Fn<A, Output = R>,
{
    type Output = R;

    extern "rust-call" fn call_once(self, args: A) -> Self::Output {
        trampoline(&self, args, &self.imp)
    }
}

impl<F, A: Tuple, R> FnMut<A> for SecurityGate<F, A, R>
where
    F: Fn<A, Output = R>,
{
    extern "rust-call" fn call_mut(&mut self, args: A) -> Self::Output {
        trampoline(&self, args, &self.imp)
    }
}

impl<F, A: Tuple, R> Fn<A> for SecurityGate<F, A, R>
where
    F: Fn<A, Output = R>,
{
    extern "rust-call" fn call(&self, args: A) -> Self::Output {
        trampoline(&self, args, &self.imp)
    }
}

/*
#[secure_gate]
fn foo(...) -> ... {...}
*/

pub static FOO_GATE: SecurityGate<fn(i32, bool) -> Option<bool>, (i32, bool), Option<bool>> =
    SecurityGate {
        imp: foo_gate_impl_trampoline,
        _pd: PhantomData,
    };

fn foo_gate_impl(x: i32, y: bool) -> Option<bool> {
    if x == 0 {
        Some(!y)
    } else {
        None
    }
}

#[link_section = ".twzsecgate"]
pub fn foo_gate_impl_trampoline(x: i32, y: bool) -> Option<bool> {
    // pre-call setup (secure callee side)
    let ret = foo_gate_impl(x, y);
    // post-call tear-down (secure callee side)
    ret
}

pub fn foo(x: i32, y: bool) -> Option<bool> {
    (FOO_GATE)(x, y)
}
