use syn::{parse_macro_input, DeriveInput, Error};

// TODO: check that the type has #[repr(C)].
// TODO: build a fingerprint for types.

#[proc_macro_derive(Invariant)]
pub fn invariant(item: proc_macro::TokenStream) -> proc_macro::TokenStream {
    match handle_invariant(parse_macro_input!(item as DeriveInput), false) {
        Ok(ts) => ts.into(),
        Err(err) => proc_macro::TokenStream::from(err.to_compile_error()),
    }
}

#[proc_macro_derive(BaseType)]
pub fn base_type(item: proc_macro::TokenStream) -> proc_macro::TokenStream {
    match handle_base_type(parse_macro_input!(item as DeriveInput)) {
        Ok(ts) => ts.into(),
        Err(err) => proc_macro::TokenStream::from(err.to_compile_error()),
    }
}

fn handle_invariant(item: DeriveInput, _copy: bool) -> Result<proc_macro2::TokenStream, Error> {
    let type_name = item.ident.clone();
    let (impl_gens, type_gens, where_clause) = item.generics.split_for_impl();
    Ok(quote::quote! {
       unsafe impl #impl_gens ::twizzler::marker::Invariant for #type_name #type_gens #where_clause {}
    })
}

fn handle_base_type(item: DeriveInput) -> Result<proc_macro2::TokenStream, Error> {
    let type_name = item.ident.clone();

    let (impl_gens, type_gens, where_clause) = item.generics.split_for_impl();
    Ok(quote::quote! {
       impl #impl_gens ::twizzler::marker::BaseType for #type_name #type_gens #where_clause {}
    })
}
