#![feature(c_str_literals)]
#![feature(iterator_try_collect)]
#![feature(proc_macro_diagnostic)]
// syn doesn't allow us to easily fix this.
#![allow(clippy::vec_box)]

use darling::FromMeta;
use proc_macro::{Diagnostic, Level};
use proc_macro2::{Ident, TokenStream};
use quote::{quote, ToTokens};
use syn::{
    parse2, parse_quote, punctuated::Punctuated, token::Pub, Attribute, BareFnArg, Error,
    ForeignItemFn, ItemFn, LitStr, Path, ReturnType, Signature, Token, Type, TypeBareFn, TypePath,
    Visibility,
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

const PREFIX: &str = "__twz_secgate_impl_";

#[allow(dead_code)]
struct Info {
    pub mod_name: Ident,
    pub fn_name: Ident,
    pub internal_fn_name: Ident,
    pub trampoline_name: Ident,
    pub entry_name: Ident,
    pub struct_name: Ident,
    pub entry_type_name: Ident,
    pub types: Vec<Box<Type>>,
    pub ret_type: ReturnType,
    pub arg_names: Vec<Ident>,
    pub has_info: bool,
}

#[derive(Debug, FromMeta)]
struct MacroArgs {
    #[darling(default)]
    options: darling::util::PathList,
}

fn build_names(
    base: Ident,
    types: Vec<Box<Type>>,
    ret_type: ReturnType,
    arg_names: Vec<Ident>,
    has_info: bool,
) -> Info {
    Info {
        mod_name: Ident::new(&format!("{}{}_mod", PREFIX, base), base.span()),
        struct_name: Ident::new(&format!("{}_info", base).to_uppercase(), base.span()),
        trampoline_name: Ident::new(&format!("{}", base), base.span()),
        entry_name: Ident::new(&format!("{}_entry", base), base.span()),
        internal_fn_name: Ident::new(&format!("{}_direct", base), base.span()),
        entry_type_name: Ident::new(&format!("{}_EntryType", base), base.span()),
        fn_name: base,
        types,
        arg_names,
        ret_type,
        has_info,
    }
}

fn handle_secure_gate(
    attr: proc_macro2::TokenStream,
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

    let arg_names: Vec<_> = tree
        .sig
        .inputs
        .iter()
        .map(|arg| match arg {
            syn::FnArg::Receiver(_) => todo!(),
            syn::FnArg::Typed(pt) => syn::parse2::<Ident>(pt.pat.to_token_stream()),
        })
        .try_collect()?;

    let attr_args = darling::ast::NestedMeta::parse_meta_list(attr)?;
    let attr_args = MacroArgs::from_list(&attr_args)?;

    let opt_info: Ident = parse_quote!(info);
    let opt_api: Ident = parse_quote!(api);

    let entry_only = attr_args.options.iter().any(|item| item.is_ident(&opt_api));

    let has_info = if attr_args
        .options
        .iter()
        .any(|item| item.is_ident(&opt_info))
    {
        if types.is_empty() {
            Diagnostic::spanned(
                tree.sig.ident.span().unwrap(),
                Level::Error,
                "option info requires at least one argument, the info struct",
            )
            .emit();
        }
        let first = tree.sig.inputs.first().unwrap();
        match first {
            syn::FnArg::Receiver(rec) => {
                Diagnostic::spanned(
                    rec.self_token.span.unwrap(),
                    Level::Error,
                    "option info may not be used on a receiver function",
                )
                .emit();
            }
            syn::FnArg::Typed(pat) => match &*pat.ty {
                Type::Reference(tr) => {
                    if tr.mutability.is_some() {
                        Diagnostic::spanned(
                            tree.sig.ident.span().unwrap(),
                            Level::Error,
                            "option info requires first argument to be immutable",
                        )
                        .emit();
                    }
                }
                _ => Diagnostic::spanned(
                    tree.sig.ident.span().unwrap(),
                    Level::Error,
                    "option info requires first argument to be a reference type to GateCallInfo",
                )
                .emit(),
            },
        }
        true
    } else {
        false
    };

    let ret_type = tree.sig.output.clone();

    let fn_name = tree.sig.ident.clone();
    let names = build_names(fn_name, types, ret_type, arg_names, has_info);
    let trampoline = build_trampoline(&tree, &names)?;
    let extern_trampoline = build_extern_trampoline(&tree, &names)?;
    let public_call_point = build_public_call(&tree, &names)?;
    let entry = build_entry(&tree, &names)?;
    let struct_def = build_struct(&tree, &names)?;
    let types_def = build_types(&tree, &names)?;

    let link_section_text: Attribute = parse_quote!(#[link_section = ".twz_secgate_text"]);
    let link_section_data: Attribute = parse_quote!(#[link_section = ".twz_secgate_info"]);

    let Info {
        mod_name,
        fn_name,
        internal_fn_name,
        ..
    } = names;
    tree.sig.ident = parse_quote!(#internal_fn_name);
    //tree.vis = parse_quote!(pub(crate));

    if entry_only {
        Ok(quote::quote! {
            pub mod #mod_name {
                use super::*;
                 #extern_trampoline
                #types_def
            }
            #public_call_point
        })
    } else {
        // For now, we put all this stuff in a mod to keep it contained.
        Ok(quote::quote! {
            // the implementation (user-written)
            #tree
            pub mod #mod_name {
                use super::*;
                pub(super) use super::#internal_fn_name;
                // the generated entry function
                #entry
                // info struct data
                #link_section_data
                #struct_def
                #types_def
                // trampoline text
                #link_section_text
                #trampoline
            }
            #public_call_point
        })
    }
}

fn get_entry_sig(tree: &ItemFn) -> Signature {
    let mut sig = tree.sig.clone();
    sig.abi = parse_quote!( extern "C" );
    sig.inputs = Punctuated::new();
    sig.inputs
        .push_value(parse_quote!(info: *const secgate::GateCallInfo));
    sig.inputs.push_punct(parse_quote!(,));
    sig.inputs.push_value(parse_quote!(args: *const Args));
    sig.inputs.push_punct(parse_quote!(,));
    sig.inputs.push_value(parse_quote!(ret: *mut Ret));
    sig.output = ReturnType::Default;

    sig
}

fn build_trampoline(tree: &ItemFn, names: &Info) -> Result<proc_macro2::TokenStream, Error> {
    let mut call_point = tree.clone();
    call_point.attrs.push(parse_quote!(#[naked]));
    call_point.attrs.push(parse_quote!(#[no_mangle]));
    call_point.vis = Visibility::Public(Pub::default());
    call_point.sig.ident = names.trampoline_name.clone();
    call_point.sig.abi = Some(syn::Abi {
        extern_token: syn::token::Extern::default(),
        name: Some(LitStr::new("C", proc_macro2::Span::mixed_site())),
    });
    let entry_sig = get_entry_sig(tree);
    call_point.sig.output = entry_sig.output;
    call_point.sig.inputs = entry_sig.inputs;

    let Info { entry_name, .. } = names;
    call_point.block = Box::new(parse2(quote::quote! {
        {
            #[cfg(target_arch = "x86_64")]
            unsafe {core::arch::asm!("jmp {0}", sym #entry_name, options(noreturn))}
            #[cfg(target_arch = "aarch64")]
            unsafe {core::arch::asm!("b {0}", sym #entry_name, options(noreturn))}
        }
    })?);

    Ok(quote::quote!(#call_point))
}

fn build_extern_trampoline(tree: &ItemFn, names: &Info) -> Result<proc_macro2::TokenStream, Error> {
    let mut entry_sig = get_entry_sig(tree);
    // This will be in an extern block.
    entry_sig.abi = None;
    entry_sig.ident = names.trampoline_name.clone();

    let ffn = ForeignItemFn {
        attrs: vec![],
        vis: Visibility::Public(Pub::default()),
        semi_token: Token![;](entry_sig.ident.span()),
        sig: entry_sig,
    };

    Ok(quote::quote!(extern "C" {#ffn}))
}

fn build_entry(tree: &ItemFn, names: &Info) -> Result<proc_macro2::TokenStream, Error> {
    let mut call_point = tree.clone();
    call_point.vis = Visibility::Inherited;
    call_point.sig = get_entry_sig(tree);

    let Info {
        entry_name,
        internal_fn_name,
        arg_names: all_arg_names,
        has_info,
        ..
    } = names;
    call_point.sig.ident = entry_name.clone();

    let arg_names = if *has_info {
        //let args = call_point.sig.inputs.into_iter().skip(1).collect();
        //call_point.sig.inputs = args;
        &all_arg_names[1..]
    } else {
        all_arg_names
    };

    let unpacked_args = if arg_names.is_empty() {
        quote! {}
    } else {
        quote! {let (#(#arg_names),*,) = unsafe {*args}.into_inner();}
    };

    let call_args = if *has_info {
        quote! {unsafe {&(*info).canonicalize()}, #(#arg_names),*}
    } else {
        quote! {#(#arg_names),*}
    };

    call_point.block = Box::new(parse2(quote::quote! {
        {
            #unpacked_args

            // Call the user-written implementation, catching unwinds.
            let impl_ret = std::panic::catch_unwind(|| #internal_fn_name(#call_args));
            // If we panic'd, report to user and return error.
            if impl_ret.is_err() {
                std::process::Termination::report(std::process::ExitCode::from(101u8));
            }
            let wret = match impl_ret {
                Ok(r) => secgate::SecGateReturn::<_>::Success(r),
                Err(_) => secgate::SecGateReturn::<_>::CalleePanic,
            };

            // Success -- write the return value.
            let ret = unsafe {ret.as_mut().unwrap()};
            ret.set(wret);
        }
    })?);

    Ok(quote::quote!(#call_point))
}

fn build_public_call(tree: &ItemFn, names: &Info) -> Result<proc_macro2::TokenStream, Error> {
    let mut call_point = tree.clone();
    call_point.attrs.push(parse_quote!(#[inline(always)]));
    call_point.vis = Visibility::Public(Pub::default());

    let ret_type = names.ret_type.clone();

    let ret_type = match ret_type {
        ReturnType::Default => Box::new(parse_quote!(())),
        ReturnType::Type(_, ty) => ty.clone(),
    };
    let rt_path: Path = parse_quote! { secgate::SecGateReturn<#ret_type> };
    call_point.sig.output = ReturnType::Type(
        Default::default(),
        Box::new(Type::Path(TypePath {
            qself: None,
            path: rt_path,
        })),
    );

    let Info {
        mod_name,
        trampoline_name,
        arg_names,
        has_info,
        ..
    } = names;

    let arg_names = if *has_info {
        let args = call_point.sig.inputs.into_iter().skip(1).collect();
        call_point.sig.inputs = args;
        &arg_names[1..]
    } else {
        arg_names
    };

    let args_tuple = if arg_names.is_empty() {
        quote! {let tuple = ();}
    } else {
        quote! {
            let tuple = (#(#arg_names),*,);
        }
    };

    call_point.block = Box::new(parse2(quote::quote! {
        {
            #args_tuple
            // Allocate stack space for args + ret. Args::with_alloca also inits the memory.
            secgate::GateCallInfo::with_alloca(0.into(), 0.into(), |info| {
                #mod_name::Args::with_alloca(tuple, |args| {
                    #mod_name::Ret::with_alloca(|ret| {
                        // Call the trampoline in the mod.
                        unsafe {
                            #mod_name::#trampoline_name(info as *const _, args as *const _, ret as *mut _);
                        }
                        ret.into_inner().unwrap_or(secgate::SecGateReturn::<_>::NoReturnValue)
                    })
                })
            })
        }
    })?);

    Ok(quote::quote!(#call_point))
}

fn build_struct(_tree: &ItemFn, names: &Info) -> Result<TokenStream, Error> {
    let Info {
        mod_name: _mod_name,
        entry_type_name,
        entry_name,
        fn_name,
        struct_name,
        ..
    } = names;

    let mut name_bytes = fn_name.to_string().into_bytes();
    name_bytes.push(0);

    let str_lit = syn::LitByteStr::new(&name_bytes, proc_macro2::Span::mixed_site());

    Ok(quote! {
        #[used]
        pub static #struct_name: secgate::SecGateInfo<&'static #entry_type_name> =
            secgate::SecGateInfo::new(&(#entry_name as #entry_type_name), unsafe {std::ffi::CStr::from_bytes_with_nul_unchecked(#str_lit)});
    })
}

fn build_types(tree: &ItemFn, names: &Info) -> Result<TokenStream, Error> {
    let Info {
        mod_name: _mod_name,
        entry_type_name,
        fn_name,
        types,
        ret_type,
        has_info,
        ..
    } = names;
    let entry_sig = get_entry_sig(tree);

    let inputs = entry_sig.inputs.into_iter().map(|arg| match arg {
        syn::FnArg::Receiver(_) => todo!(),
        syn::FnArg::Typed(ty) => ty.ty,
    });

    let inputs = Punctuated::from_iter(inputs.map(|ty| BareFnArg {
        attrs: vec![],
        name: None,
        ty: *ty,
    }));

    let ty = TypeBareFn {
        lifetimes: None,
        unsafety: entry_sig.unsafety,
        abi: entry_sig.abi,
        fn_token: entry_sig.fn_token,
        paren_token: entry_sig.paren_token,
        inputs,
        variadic: None,
        output: entry_sig.output,
    };

    let ret_type = match ret_type {
        ReturnType::Default => Box::new(parse_quote!(())),
        ReturnType::Type(_, ty) => ty.clone(),
    };

    let mut name_bytes = fn_name.to_string().into_bytes();
    name_bytes.push(0);

    let types = if *has_info { &types[1..] } else { types };

    let arg_types = if types.is_empty() {
        quote! {secgate::Arguments<()>}
    } else {
        quote! {
            secgate::Arguments<(#(#types),*,)>
        }
    };

    Ok(quote! {
        #[allow(non_camel_case_types)]
        type #entry_type_name = #ty;
        pub type Args = #arg_types;
        pub type Ret = secgate::Return<secgate::SecGateReturn<#ret_type>>;
        pub const ARGS_SIZE: usize = core::mem::size_of::<Args>();
        pub const RET_SIZE: usize = core::mem::size_of::<Ret>();
    })
}
