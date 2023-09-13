#![feature(fn_traits)]
#![feature(unboxed_closures)]
#![feature(tuple_trait)]
#![feature(naked_functions)]
#![feature(asm_sym)]

use std::marker::{PhantomData, Tuple};

pub use secgate_macros::*;

pub struct SecurityGate<Imp, Args, Ret> {
    imp: Imp,
    _pd: PhantomData<(Imp, Args, Ret)>,
}

impl<Imp, Args, Ret> SecurityGate<Imp, Args, Ret> {
    pub const fn new(imp: Imp) -> Self {
        Self {
            imp,
            _pd: PhantomData,
        }
    }
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
