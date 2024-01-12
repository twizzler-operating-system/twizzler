#![feature(c_str_literals)]
#![feature(iterator_try_collect)]
use proc_macro2::{Ident, TokenStream};
use quote::{quote, ToTokens};
use syn::{
    parse2, parse_quote, punctuated::Punctuated, token::Pub, Attribute, BareFnArg, Error, ItemFn,
    LitStr, Path, ReturnType, Signature, Type, TypeBareFn, TypePath, Visibility,
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

const PREFIX: &'static str = "__twz_secgate_impl_";

#[allow(dead_code)]
struct Info {
    pub mod_name: Ident,
    pub fn_name: Ident,
    pub trampoline_name: Ident,
    pub entry_name: Ident,
    pub struct_name: Ident,
    pub entry_type_name: Ident,
    pub types: Vec<Box<Type>>,
    pub ret_type: Option<Box<Type>>,
    pub arg_names: Vec<Ident>,
}

fn build_names(
    base: Ident,
    types: Vec<Box<Type>>,
    ret_type: ReturnType,
    arg_names: Vec<Ident>,
) -> Info {
    let x = match ret_type {
        ReturnType::Default => None,
        ReturnType::Type(_, ty) => Some(ty),
    };
    Info {
        mod_name: Ident::new(&format!("{}{}_mod", PREFIX, base), base.span()),
        struct_name: Ident::new(&format!("{}_info", base).to_uppercase(), base.span()),
        trampoline_name: Ident::new(&format!("{}_trampoline", base), base.span()),
        entry_name: Ident::new(&format!("{}_entry", base), base.span()),
        entry_type_name: Ident::new(&format!("{}_EntryType", base), base.span()),
        fn_name: base,
        types,
        arg_names,
        ret_type: x,
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

    let arg_names: Vec<_> = tree
        .sig
        .inputs
        .iter()
        .map(|arg| match arg {
            syn::FnArg::Receiver(_) => todo!(),
            syn::FnArg::Typed(pt) => syn::parse2::<Ident>(pt.pat.to_token_stream()),
        })
        .try_collect()?;

    let ret_type = tree.sig.output.clone();

    let fn_name = tree.sig.ident.clone();
    let names = build_names(fn_name, types, ret_type, arg_names);
    let trampoline = build_trampoline(&tree, &names)?;
    let public_call_point = build_public_call(&tree, &names)?;
    let entry = build_entry(&tree, &names)?;
    let struct_def = build_struct(&tree, &names)?;

    let link_section_text: Attribute = parse_quote!(#[link_section = ".twz_secgate_text"]);
    let link_section_data: Attribute = parse_quote!(#[link_section = ".twz_secgate_info"]);

    let Info {
        mod_name,
        fn_name: _fn_name,
        struct_name: _struct_name,
        ..
    } = names;
    tree.vis = Visibility::Inherited;
    Ok(quote::quote! {
        pub mod #mod_name {
            #tree
            #entry
            #link_section_data
            #struct_def
            #link_section_text
            #trampoline
        }
        #public_call_point
    })
}

fn get_entry_sig(tree: &ItemFn) -> Signature {
    let mut sig = tree.sig.clone();
    sig.abi = parse_quote!( extern "C" );
    sig.inputs = Punctuated::new();
    sig.inputs.push_value(parse_quote!(args: *const Args));
    sig.inputs.push_punct(parse_quote!(,));
    sig.inputs.push_value(parse_quote!(ret: *mut Ret));
    sig.output = ReturnType::Default;

    sig
}

fn build_trampoline(tree: &ItemFn, names: &Info) -> Result<proc_macro2::TokenStream, Error> {
    let mut call_point = tree.clone();
    call_point.attrs.push(parse_quote!(#[naked]));
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
            unsafe {core::arch::asm!("jmp {0}", sym #entry_name, options(noreturn))}
        }
    })?);

    Ok(quote::quote!(#call_point))
}

fn build_entry(tree: &ItemFn, names: &Info) -> Result<proc_macro2::TokenStream, Error> {
    let mut call_point = tree.clone();
    call_point.vis = Visibility::Inherited;
    call_point.sig = get_entry_sig(tree);

    let Info {
        entry_name,
        fn_name,
        arg_names,
        ..
    } = names;
    call_point.sig.ident = entry_name.clone();
    call_point.block = Box::new(parse2(quote::quote! {
        {
            let (#(#arg_names),*) = unsafe {*args}.into_inner();
            //do_setup();
            let impl_ret = std::panic::catch_unwind(|| #fn_name(#(#arg_names),*));
            if impl_ret.is_err() {
                std::process::Termination::report(std::process::ExitCode::from(101u8));
            }
            //do_teardown();
            let wret = match impl_ret {
                Ok(r) => secgate::SecGateReturn::Success(r),
                Err(_) => secgate::SecGateReturn::CalleePanic,
            };

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

    let ret_type = names.ret_type.clone().unwrap_or_else(|| parse_quote!(()));
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
        ..
    } = names;
    call_point.block = Box::new(parse2(quote::quote! {
        {
            let tuple = (#(#arg_names),*);
            #mod_name::Args::with_alloca(tuple, |args| {
                #mod_name::Ret::with_alloca(|ret| {
                    #mod_name::#trampoline_name(args as *const _, ret as *mut _);
                    ret.into_inner().unwrap_or(secgate::SecGateReturn::NoReturnValue)
                })
            })
        }
    })?);

    Ok(quote::quote!(#call_point))
}

fn build_struct(tree: &ItemFn, names: &Info) -> Result<TokenStream, Error> {
    let Info {
        mod_name: _mod_name,
        entry_type_name,
        entry_name,
        fn_name,
        struct_name,
        types,
        ret_type,
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

    let mut name_bytes = fn_name.to_string().into_bytes();
    name_bytes.push(0);

    let str_lit = syn::LitByteStr::new(&name_bytes, proc_macro2::Span::mixed_site());

    Ok(quote! {
        #[used]
        pub static #struct_name: secgate::SecGateInfo<&'static #entry_type_name> =
            secgate::SecGateInfo::new(&(#entry_name as #entry_type_name), unsafe {std::ffi::CStr::from_bytes_with_nul_unchecked(#str_lit)});
        #[allow(non_camel_case_types)]
        type #entry_type_name = #ty;
        pub type Args = secgate::Arguments<(#(#types),*)>;
        pub type Ret = secgate::Return<secgate::SecGateReturn<#ret_type>>;
        pub const ARGS_SIZE: usize = core::mem::size_of::<Args>();
        pub const RET_SIZE: usize = core::mem::size_of::<Ret>();
    })
}
