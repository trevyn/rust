use std::num::*;

#[repr(C)]
struct S1(NonZeroI32);

#[repr(C)]
struct S2(i32);

fn callee(_s: S2) {}

fn main() {
    let fnptr: fn(S2) = callee;
    let fnptr: fn(S1) = unsafe { std::mem::transmute(fnptr) };
    fnptr(S1(NonZeroI32::new(1).unwrap()));
    //~^ ERROR: calling a function with argument of type S2 passing data of type S1
}
