use proc_macro2::{Ident, TokenStream};
use quote::quote;
use syn::{
    parse2,
    punctuated::Punctuated,
    token::{Bracket, Paren, Pound, Pub},
    Attribute, Error, Expr, ExprLit, ItemFn, LitStr, MetaNameValue, Path, PathSegment, ReturnType,
    Type, VisRestricted, Visibility,
};

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

const PREFIX: &'static str = "__twz_gate_imp_";

struct Info {
    pub mod_name: Ident,
    pub fn_name: Ident,
    pub struct_name: Ident,
    pub types: Vec<Box<Type>>,
    pub ret_type: Option<Box<Type>>,
}

fn build_names(base: Ident, types: Vec<Box<Type>>, ret_type: ReturnType) -> Info {
    let x = match ret_type {
        ReturnType::Default => None,
        ReturnType::Type(_, ty) => Some(ty),
    };
    Info {
        mod_name: Ident::new(&format!("{}{}_mod", PREFIX, base), base.span()),
        struct_name: Ident::new(
            &format!("{}{}_struct", PREFIX, base).to_uppercase(),
            base.span(),
        ),
        fn_name: base,
        types,
        ret_type: x,
    }
}

fn link_section_attr(info: &Info, section: &str) -> Attribute {
    Attribute {
        style: syn::AttrStyle::Outer,
        meta: syn::Meta::NameValue(MetaNameValue {
            path: Path {
                segments: Punctuated::from_iter([PathSegment {
                    ident: Ident::new("link_section", info.fn_name.span()),
                    arguments: syn::PathArguments::None,
                }]),
                leading_colon: None,
            },
            value: Expr::Lit(ExprLit {
                lit: syn::Lit::Str(LitStr::new(section, info.fn_name.span())),
                attrs: vec![],
            }),
            eq_token: syn::token::Eq(info.fn_name.span()),
        }),
        pound_token: Pound::default(),
        bracket_token: Bracket::default(),
    }
}

fn handle_secure_gate(
    _attr: proc_macro2::TokenStream,
    item: proc_macro2::TokenStream,
) -> Result<proc_macro2::TokenStream, Error> {
    let mut tree = syn::parse2::<syn::ItemFn>(item)?;

    let types: Vec<_> = tree
        .sig
        .inputs
        .iter()
        .map(|arg| match arg {
            syn::FnArg::Receiver(_) => todo!(),
            syn::FnArg::Typed(pt) => pt.ty.clone(),
        })
        .collect();

    let ret_type = tree.sig.output.clone();

    let fn_name = tree.sig.ident.clone();
    let names = build_names(fn_name, types, ret_type);
    let public_call_point = build_call_point(&tree, &names)?;
    let struct_def = build_struct(&tree, &names)?;

    let link_section_text = link_section_attr(&names, ".twz_gate_text");
    let link_section_data = link_section_attr(&names, ".twz_gate_data");

    let Info {
        mod_name,
        fn_name: _fn_name,
        struct_name: _struct_name,
        ..
    } = names;
    tree.vis = Visibility::Restricted(VisRestricted {
        pub_token: Pub(tree.sig.ident.span()),
        paren_token: Paren::default(),
        in_token: None,
        path: Box::new(parse2(quote!(super))?),
    });
    Ok(quote::quote! {
        mod #mod_name {
            #tree
            #link_section_data
            #struct_def
        }
        #link_section_text
        #public_call_point
    })
}

fn parse_outer_attribute(attr: TokenStream) -> Result<Attribute, Error> {
    Ok(Attribute {
        pound_token: Pound::default(),
        style: syn::AttrStyle::Outer,
        bracket_token: Bracket::default(),
        meta: syn::Meta::Path(parse2(attr)?),
    })
}

fn build_call_point(tree: &ItemFn, names: &Info) -> Result<proc_macro2::TokenStream, Error> {
    let mut call_point = tree.clone();
    call_point.attrs.push(parse_outer_attribute(quote!(naked))?);
    call_point.vis = Visibility::Public(Pub::default());

    let Info {
        mod_name,
        fn_name,
        struct_name: _struct_name,
        ..
    } = names;
    call_point.block = Box::new(parse2(quote::quote! {
        {
            unsafe {core::arch::asm!("jmp {0}", sym #mod_name::#fn_name, options(noreturn))}
        }
    })?);

    Ok(quote::quote!(#call_point))
}

fn build_struct(_tree: &ItemFn, names: &Info) -> Result<TokenStream, Error> {
    let Info {
        mod_name: _mod_name,
        fn_name,
        struct_name,
        types,
        ret_type,
        ..
    } = names;
    let ret_type = ret_type.clone().unwrap_or(parse2(quote!(()))?);
    Ok(quote! {
        pub static #struct_name: crate::SecurityGate<Imp, Args, Ret> = crate::SecurityGate::new(super::#fn_name);

        pub type Imp = fn(#(#types),*) -> Ret;
        pub type Args = (#(#types),*);
        pub type Ret = #ret_type;
    })
}
