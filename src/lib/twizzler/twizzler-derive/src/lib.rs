use std::{any::type_name, hash::BuildHasher};

use proc_macro2::{Ident, Span};
use quote::quote;
use syn::{parse_macro_input, spanned::Spanned, DeriveInput, Error};

#[proc_macro_derive(Invariant)]
pub fn invariant(item: proc_macro::TokenStream) -> proc_macro::TokenStream {
    match handle_invariant(parse_macro_input!(item as DeriveInput), false) {
        Ok(ts) => ts.into(),
        Err(err) => proc_macro::TokenStream::from(err.to_compile_error()),
    }
}

#[proc_macro_derive(InvariantCopy)]
pub fn invariant_copy(item: proc_macro::TokenStream) -> proc_macro::TokenStream {
    match handle_invariant(parse_macro_input!(item as DeriveInput), true) {
        Ok(ts) => ts.into(),
        Err(err) => proc_macro::TokenStream::from(err.to_compile_error()),
    }
}

fn handle_invariant(item: DeriveInput, copy: bool) -> Result<proc_macro2::TokenStream, Error> {
    let type_name = item.ident.clone();
    let mut in_place_vec = vec![];
    let mut builder_vec = vec![];
    let mut new_vec = vec![];
    match item.data {
        syn::Data::Struct(st) => {
            let fields = st.fields.iter().enumerate().map(|f| {
                (
                    if let Some(ref name) = f.1.ident {
                        name.clone()
                    } else {
                        Ident::new(&f.0.to_string(), f.1.span())
                    },
                    f.1.ty.clone(),
                )
            });
            for (f, t) in fields {
                in_place_vec.push(quote! {
                    unsafe {
                    let ptr = place as *mut _ as *mut Self;
                    let ptr = core::ptr::addr_of!((*ptr).#f) as *mut core::mem::MaybeUninit<#t>;
                        <#t>::in_place_ctor(builder.#f, &mut *ptr, &tx)?;
                    }
                });
                builder_vec.push(quote! {
                    #f: <#t as twizzler::marker::InPlaceCtor>::Builder,
                });
                new_vec.push(quote! {
                    #f,
                });
            }
        }
        syn::Data::Enum(_) => todo!(),
        syn::Data::Union(_) => todo!(),
    }
    let builder_name = Ident::new(
        &format!("{}Builder", type_name.to_string()),
        type_name.span(),
    );
    let in_place = if copy {
        quote! {}
    } else {
        quote! {}
    };
    Ok(quote::quote! {
       unsafe impl twizzler::marker::Invariant for #type_name {}

       #in_place

    })
}
