#![feature(extend_one)]

use proc_macro::{TokenStream, TokenTree};
extern crate proc_macro;

#[proc_macro_attribute]
// Okay, look, I know what you're gonna say. Why do we need to get this complicated just to do
// tests. The answer is names. See, our friends in the rust community have not fully implemented
// this issue: https://github.com/rust-lang/rust/issues/50297. Until this is implemented, I don't know how to grab
// name from a test function in a way that makes the test _runner_ know the names of the tests it's
// running. So we just embed the name ourselves using #[test_case].
pub fn kernel_test(_attr: TokenStream, items: TokenStream) -> TokenStream {
    let mut out = TokenStream::new();
    let mut it = items.into_iter();
    let mut name = None;
    // Extract the test name
    loop {
        let item = it.next();
        if let Some(item) = item {
            if matches!(item, TokenTree::Ident(ref ident) if ident.to_string() == "fn") {
                out.extend_one(item);
                let fname = it.next().unwrap();
                name = Some(fname.to_string());
                out.extend_one(fname);
            } else {
                out.extend_one(item);
            }
        } else {
            break;
        }
    }
    let name = name.unwrap_or("unknown".to_string());
    // Write the test caller and the name into a test_case tuple
    let mut code: TokenStream = format!(
        "#[test_case] const __X_{}: (&'static str, &'static dyn Fn()) = (\"{}\", &|| {{ {}(); }});",
        name.to_uppercase(),
        name,
        name
    )
    .parse()
    .unwrap();
    code.extend(out);
    code
}
