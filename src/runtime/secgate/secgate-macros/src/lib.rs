use syn::Error;

#[proc_macro_attribute]
pub fn secure_gate(
    attr: proc_macro::TokenStream,
    item: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    match handle_secure_gate(attr.into(), item.into()) {
        Ok(ts) => ts.into(),
        Err(err) => proc_macro::TokenStream::from(err.to_compile_error()),
    }
}

fn handle_secure_gate(
    _attr: proc_macro2::TokenStream,
    item: proc_macro2::TokenStream,
) -> Result<proc_macro2::TokenStream, Error> {
    let tree = syn::parse2::<syn::ItemFn>(item)?;
    Ok(quote::quote! {#tree})
}
