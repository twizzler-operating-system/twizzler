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

#[proc_macro_derive(BaseType)]
pub fn base_type(item: proc_macro::TokenStream) -> proc_macro::TokenStream {
    match handle_base_type(parse_macro_input!(item as DeriveInput)) {
        Ok(ts) => ts.into(),
        Err(err) => proc_macro::TokenStream::from(err.to_compile_error()),
    }
}

#[proc_macro_derive(NewStorer)]
pub fn new_storer(item: proc_macro::TokenStream) -> proc_macro::TokenStream {
    match handle_new_storer(parse_macro_input!(item as DeriveInput)) {
        Ok(ts) => ts.into(),
        Err(err) => proc_macro::TokenStream::from(err.to_compile_error()),
    }
}

fn handle_invariant(item: DeriveInput, copy: bool) -> Result<proc_macro2::TokenStream, Error> {
    let type_name = item.ident.clone();
    /*
    //let mut in_place_vec = vec![];
    //let mut builder_vec = vec![];
    //let mut new_vec = vec![];
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
                /*in_place_vec.push(quote! {
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
                });*/
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
    */
    let (impl_gens, type_gens, where_clause) = item.generics.split_for_impl();
    Ok(quote::quote! {
       unsafe impl #impl_gens twizzler::marker::Invariant for #type_name #type_gens #where_clause {}
    })
}

fn handle_base_type(item: DeriveInput) -> Result<proc_macro2::TokenStream, Error> {
    let type_name = item.ident.clone();

    let (impl_gens, type_gens, where_clause) = item.generics.split_for_impl();
    Ok(quote::quote! {
       impl #impl_gens twizzler::marker::BaseType for #type_name #type_gens #where_clause {}
    })
}

fn handle_new_storer(item: DeriveInput) -> Result<proc_macro2::TokenStream, Error> {
    let type_name = item.ident.clone();
    let (impl_gens, type_gens, where_clause) = item.generics.split_for_impl();

    let types: Vec<_> = match item.data {
        syn::Data::Struct(st) => st
            .fields
            .iter()
            .enumerate()
            .map(|f| {
                (
                    f.1.ident
                        .clone()
                        .unwrap_or(Ident::new(&format!("x{}", f.0), f.1.ident.span())),
                    f.1.ty.clone(),
                )
            })
            .collect(),
        syn::Data::Enum(_) => todo!(),
        syn::Data::Union(_) => todo!(),
    };

    let params = types
        .iter()
        .map(|(ident, ty)| quote!(#ident: impl Into<twizzler::marker::Storer<#ty>>));

    let inits = types
        .iter()
        .map(|(ident, _ty)| quote!(#ident: #ident.into().into_inner()));

    Ok(quote::quote! {
       impl #impl_gens #type_name #type_gens #where_clause {
           pub fn new_storer(#(#params),*) -> twizzler::marker::Storer<Self> {
               unsafe {
                   twizzler::marker::Storer::new_move(
                       Self {
                            #(#inits),*
                       }
                   )
               }
           }
       }
    })
}
