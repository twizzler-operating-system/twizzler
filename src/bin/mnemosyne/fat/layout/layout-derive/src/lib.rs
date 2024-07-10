use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{parse_macro_input, Ident, ItemStruct, Type};

enum FieldLayout {
    SubLayout,
    Primitive,
}

impl FieldLayout {
    fn gen_accessor(
        &self,
        offset_in_parent: &TokenStream2,
        name: &Ident,
        ty: &Type,
    ) -> TokenStream2 {
        match self {
            FieldLayout::SubLayout => {
                let all_name = Ident::new(&format!("all_{name}"), name.span());
                quote! {
                    pub fn #name (&mut self) -> Result<<#ty as ::layout::ApplyLayout<'_, R>>::Frame, R::Error> {
                        let offset_in_parent = #offset_in_parent;
                        <#ty as ::layout::ApplyLayout<'_, R>>::apply_layout(self.stream, self.offset + offset_in_parent)
                    }

                    pub fn #all_name (&mut self) -> Result<#ty, R::Error> {
                        let offset_in_parent = #offset_in_parent;
                        self.stream.seek(::layout::io::SeekFrom::Start(self.offset + offset_in_parent))?;
                        <#ty as ::layout::Decode>::decode(self.stream)
                    }
                }
            }
            FieldLayout::Primitive => quote! {
                pub fn #name (&mut self) -> Result<#ty, R::Error> {
                    let offset_in_parent = #offset_in_parent;
                    self.stream.seek(::layout::io::SeekFrom::Start(self.offset + offset_in_parent))?;
                    <#ty as ::layout::Decode>::decode(self.stream)
                }
            },
        }
    }

    fn gen_setter(&self, offset_in_parent: &TokenStream2, name: &Ident, ty: &Type) -> TokenStream2 {
        let setter_name = Ident::new(&format!("set_{name}"), name.span());
        quote! {
            pub fn #setter_name (&mut self, data: &#ty) -> Result<(), W::Error> {
                let offset_in_parent = #offset_in_parent;
                self.stream.seek(::layout::io::SeekFrom::Start(self.offset + offset_in_parent))?;
                <#ty as ::layout::Encode>::encode(data, self.stream)
            }
        }
    }
}

enum Size {
    Dynamic,
    Fixed,
}

impl Size {
    fn gen_field_size(&self, name: &Ident, ty: &Type) -> [TokenStream2; 3] {
        match self {
            Size::Dynamic => [
                quote!(::layout::FramedDynamic::framed_size(&mut self.#name()?)?),
                quote!(::layout::SourcedDynamic::sourced_size(&#name)),
                quote!(::layout::SourcedDynamic::sourced_size(&self.#name)),
            ],
            Size::Fixed => {
                let s = quote!(<#ty as ::layout::Fixed>::size());
                [s.clone(), s.clone(), s]
            }
        }
    }

    fn gen_impl_wrapper(
        &self,
        framed: TokenStream2,
        sourced: TokenStream2,
        name: &Ident,
        frame_name: &Ident,
    ) -> TokenStream2 {
        match self {
            Size::Dynamic => quote! {
                impl<'a, S> ::layout::FramedDynamic<S> for #frame_name<'a, S>
                where
                    S: ::layout::Read + ::layout::Seek + ::layout::IO
                {
                    fn framed_size(&mut self) -> Result<u64, S::Error> {
                        Ok(#framed)
                    }
                    
                }

                impl ::layout::SourcedDynamic for #name {
                    fn sourced_size(&self) -> u64 {
                        #sourced
                    }
                }
            },
            Size::Fixed => quote! {
                impl ::layout::Fixed for #name {
                    fn size() -> u64 {
                        #framed
                    }
                }
            },
        }
    }
}

#[proc_macro_attribute]
pub fn layout(_: TokenStream, item: TokenStream) -> TokenStream {
    let mut tokens = parse_macro_input!(item as ItemStruct);

    let vis = &tokens.vis;
    let source_name = &tokens.ident;
    let frame_name = Ident::new(&format!("{}Frame", tokens.ident), tokens.ident.span());

    let mut accessor_impls = TokenStream2::new();
    let mut setter_impls = TokenStream2::new();

    let mut encode_impl = TokenStream2::new();
    let mut decode_impl_assigns = TokenStream2::new();
    let mut decode_impl_struct_init = TokenStream2::new();

    let mut total_framed_offset: TokenStream2 = quote!(0);
    let mut total_sourced_offset: TokenStream2 = quote!(0);
    let mut total_selfed_offset: TokenStream2 = quote!(0);

    let mut size = Size::Fixed;

    'outer: for field in &mut tokens.fields {
        let mut field_layout = FieldLayout::Primitive;
        let mut field_size = Size::Fixed;

        for attr in field.attrs.drain(..) {
            let p = attr.path;
            match quote!(#p).to_string().as_str() {
                "sublayout" => field_layout = FieldLayout::SubLayout,
                "dynamic" => {
                    field_size = Size::Dynamic;
                    size = Size::Dynamic;
                    field_layout = FieldLayout::SubLayout;
                }
                "ignore" => continue 'outer,
                _ => continue,
            }
        }

        let field_name = field.ident.as_ref().unwrap();
        let field_ty = &field.ty;

        let accessor = field_layout.gen_accessor(&total_framed_offset, field_name, field_ty);
        accessor_impls = quote!(#accessor_impls #accessor);

        let setter = field_layout.gen_setter(&total_framed_offset, field_name, field_ty);
        setter_impls = quote!(#setter_impls #setter);

        encode_impl = quote!(#encode_impl self.#field_name.encode(writer)?;);

        decode_impl_assigns = quote!(#decode_impl_assigns let #field_name = <#field_ty>::decode(reader)?;);
        decode_impl_struct_init = quote!(#decode_impl_struct_init #field_name,);

        let [framed, sourced, selfed] = field_size.gen_field_size(field_name, field_ty);
        total_framed_offset = quote!(#total_framed_offset + #framed);
        total_sourced_offset = quote!(#total_sourced_offset + #sourced);
        total_selfed_offset = quote!(#total_selfed_offset + #selfed);
    }

    let size_impl = size.gen_impl_wrapper(
        total_framed_offset,
        total_selfed_offset,
        source_name,
        &frame_name,
    );

    // todo: already existent generics
    let out = quote! {
        #tokens

        impl<'a, R: 'a + ::layout::IO> ::layout::ApplyLayout<'a, R> for #source_name {
            type Frame = #frame_name<'a, R>;

            fn apply_layout(stream: &'a mut R, offset: u64) -> Result<Self::Frame, R::Error> {
                Ok(#frame_name {
                    stream,
                    offset
                })
            }
        }

        impl ::layout::Encode for #source_name {
            fn encode<S: ::layout::Write + ::layout::Seek + ::layout::IO>(&self, writer: &mut S) -> Result<(), S::Error> {
                #encode_impl
                Ok(())
            }
        }

        impl ::layout::Decode for #source_name {
            fn decode<S: ::layout::Read + ::layout::Seek + ::layout::IO>(reader: &mut S) -> Result<Self, S::Error> {
                #decode_impl_assigns

                Ok(Self {
                    #decode_impl_struct_init
                })
            }
        }

        #vis struct #frame_name<'a, R> {
            stream: &'a mut R,
            offset: u64
        }

        #size_impl

        impl<'a, R> ::layout::Frame<R> for #frame_name<'a, R> {
            fn stream(&mut self) -> &mut R {
                self.stream
            }

            fn offset(&self) -> u64 {
                self.offset
            }
        }

        impl<'a, R: ::layout::Read + ::layout::Seek + ::layout::IO> #frame_name<'a, R> {
            #accessor_impls
        }

        impl<'a, W: ::layout::Write + ::layout::Read + ::layout::Seek + ::layout::IO> #frame_name<'a, W> {
            #setter_impls
        }
    };

    // println!("{out}");

    out.into()
}
