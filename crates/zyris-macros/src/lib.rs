use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::{
    Error, FnArg, Ident, ItemTrait, LitInt, LitStr, Pat, PathArguments, ReturnType, Token,
    TraitItem, TraitItemFn, Type,
};

struct CapabilityArgs {
    name: LitStr,
    version: LitInt,
}

impl Parse for CapabilityArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut name = None;
        let mut version = None;
        while !input.is_empty() {
            let key: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            match key.to_string().as_str() {
                "name" => name = Some(input.parse::<LitStr>()?),
                "version" => version = Some(input.parse::<LitInt>()?),
                other => {
                    return Err(Error::new(
                        key.span(),
                        format!(
                            "unknown capability attribute `{other}`; expected `name` or `version`"
                        ),
                    ))
                }
            }
            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }
        let name = name.ok_or_else(|| Error::new(input.span(), "missing `name = \"...\"`"))?;
        let version = version.ok_or_else(|| Error::new(input.span(), "missing `version = N`"))?;
        Ok(CapabilityArgs { name, version })
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Transfer {
    Unary,
    UniStream,
}

struct ToolMethod {
    ident: Ident,
    description: String,
    transfer: Transfer,
    args: Vec<(Ident, Type)>,
    response: Type,
    stream_item: Option<Type>,
    request_struct: Ident,
}

#[proc_macro_attribute]
pub fn capability(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = syn::parse_macro_input!(attr as CapabilityArgs);
    let mut item_trait = syn::parse_macro_input!(item as ItemTrait);
    match expand(args, &mut item_trait) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

fn expand(args: CapabilityArgs, item_trait: &mut ItemTrait) -> syn::Result<TokenStream2> {
    if !item_trait.generics.params.is_empty() {
        return Err(Error::new(
            item_trait.generics.span(),
            "capability traits cannot have generic parameters",
        ));
    }
    if !item_trait.supertraits.is_empty() {
        return Err(Error::new(
            item_trait.supertraits.span(),
            "capability traits cannot have supertraits",
        ));
    }

    let trait_ident = item_trait.ident.clone();
    let vis = item_trait.vis.clone();
    let cap_name = args.name.value();
    let version: u32 = args.version.base10_parse()?;

    let mut methods = Vec::new();
    for item in &mut item_trait.items {
        let TraitItem::Fn(method) = item else {
            return Err(Error::new(item.span(), "capability traits may only contain methods"));
        };
        methods.push(parse_method(&trait_ident, method)?);
    }

    let request_structs: Vec<_> = methods.iter().map(|m| request_struct(&vis, m)).collect();
    let descriptor_fn_ident =
        format_ident!("{}_capability", snake_case(&trait_ident.to_string()));
    let descriptor = descriptor_fn(&vis, &descriptor_fn_ident, &cap_name, version, &methods);
    let server = server_impl(&vis, &trait_ident, &descriptor_fn_ident, &cap_name, &methods);
    let client = client_impl(&vis, &trait_ident, &cap_name, version, &methods);
    let rewritten = rewrite_trait(item_trait);

    Ok(quote! {
        #rewritten
        #(#request_structs)*
        #descriptor
        #server
        #client
    })
}

fn parse_method(trait_ident: &Ident, method: &mut TraitItemFn) -> syn::Result<ToolMethod> {
    if method.default.is_some() {
        return Err(Error::new(method.span(), "capability methods cannot have default bodies"));
    }
    if method.sig.asyncness.is_none() {
        return Err(Error::new(method.sig.span(), "capability methods must be async"));
    }
    if !method.sig.generics.params.is_empty() || method.sig.generics.where_clause.is_some() {
        return Err(Error::new(
            method.sig.generics.span(),
            "capability methods cannot be generic",
        ));
    }

    let mut transfer = Transfer::Unary;
    let mut zyris_attrs = Vec::new();
    method.attrs.retain(|attr| {
        if attr.path().is_ident("zyris") {
            zyris_attrs.push(attr.clone());
            false
        } else {
            true
        }
    });
    for attr in zyris_attrs {
        let mode: Ident = attr.parse_args()?;
        match mode.to_string().as_str() {
            "uni_stream" => transfer = Transfer::UniStream,
            "bi_stream" => {
                return Err(Error::new(mode.span(), "bi_stream is reserved in Zyris protocol v1"))
            }
            "video_stream" => {
                return Err(Error::new(mode.span(), "video_stream tools are not supported yet"))
            }
            other => {
                return Err(Error::new(
                    mode.span(),
                    format!("unknown transfer mechanism `{other}`"),
                ))
            }
        }
    }

    let mut inputs = method.sig.inputs.iter();
    match inputs.next() {
        Some(FnArg::Receiver(receiver))
            if matches!(receiver.kind, syn::ReceiverKind::Reference { .. })
                && receiver.mutability.is_none() => {}
        _ => return Err(Error::new(method.sig.span(), "capability methods must take `&self`")),
    }
    let mut args = Vec::new();
    for input in inputs {
        let FnArg::Typed(pat_type) = input else {
            return Err(Error::new(input.span(), "unexpected receiver"));
        };
        let Pat::Ident(pat_ident) = pat_type.pat.as_ref() else {
            return Err(Error::new(
                pat_type.pat.span(),
                "capability method arguments must be plain identifiers",
            ));
        };
        args.push((pat_ident.ident.clone(), (*pat_type.ty).clone()));
    }

    let ok_type = result_ok_type(&method.sig.output)?;
    let (response, stream_item) = match transfer {
        Transfer::Unary => (ok_type, None),
        Transfer::UniStream => {
            let (head, item) = streaming_types(&ok_type)?;
            (head, Some(item))
        }
    };

    let description = doc_string(&method.attrs);
    let request_struct = format_ident!(
        "{}{}Request",
        trait_ident,
        pascal_case(&method.sig.ident.to_string())
    );

    Ok(ToolMethod {
        ident: method.sig.ident.clone(),
        description,
        transfer,
        args,
        response,
        stream_item,
        request_struct,
    })
}

fn result_ok_type(output: &ReturnType) -> syn::Result<Type> {
    let ReturnType::Type(_, ty) = output else {
        return Err(Error::new(
            output.span(),
            "capability methods must return zyris::Result<T>",
        ));
    };
    let Type::Path(type_path) = ty.as_ref() else {
        return Err(Error::new(ty.span(), "capability methods must return zyris::Result<T>"));
    };
    let last = type_path
        .path
        .segments
        .last()
        .ok_or_else(|| Error::new(ty.span(), "invalid return type"))?;
    if last.ident != "Result" {
        return Err(Error::new(ty.span(), "capability methods must return zyris::Result<T>"));
    }
    let PathArguments::AngleBracketed(generics) = &last.arguments else {
        return Err(Error::new(ty.span(), "Result must have a type parameter"));
    };
    let mut types = generics.args.iter().filter_map(|arg| match arg {
        syn::GenericArgument::Type(t) => Some(t.clone()),
        _ => None,
    });
    let ok = types
        .next()
        .ok_or_else(|| Error::new(ty.span(), "Result must have a type parameter"))?;
    if types.next().is_some() {
        return Err(Error::new(
            ty.span(),
            "capability methods must use zyris::Result with the default error type",
        ));
    }
    Ok(ok)
}

fn streaming_types(ty: &Type) -> syn::Result<(Type, Type)> {
    let err = || {
        Error::new(
            ty.span(),
            "uni_stream methods must return zyris::Result<zyris::Streaming<Head, Item>>",
        )
    };
    let Type::Path(type_path) = ty else { return Err(err()) };
    let last = type_path.path.segments.last().ok_or_else(err)?;
    if last.ident != "Streaming" {
        return Err(err());
    }
    let PathArguments::AngleBracketed(generics) = &last.arguments else {
        return Err(err());
    };
    let mut types = generics.args.iter().filter_map(|arg| match arg {
        syn::GenericArgument::Type(t) => Some(t.clone()),
        _ => None,
    });
    let head = types.next().ok_or_else(err)?;
    let item = types.next().ok_or_else(err)?;
    Ok((head, item))
}

fn doc_string(attrs: &[syn::Attribute]) -> String {
    let mut lines = Vec::new();
    for attr in attrs {
        if attr.path().is_ident("doc") {
            if let syn::Meta::NameValue(nv) = &attr.meta {
                if let syn::Expr::Lit(expr_lit) = &nv.value {
                    if let syn::Lit::Str(s) = &expr_lit.lit {
                        lines.push(s.value().trim().to_string());
                    }
                }
            }
        }
    }
    lines.join("\n")
}

fn rewrite_trait(item_trait: &ItemTrait) -> TokenStream2 {
    let mut rewritten = item_trait.clone();
    rewritten.supertraits.push(syn::parse_quote!(::core::marker::Send));
    rewritten.supertraits.push(syn::parse_quote!(::core::marker::Sync));
    rewritten.supertraits.push(syn::parse_quote!('static));
    quote! {
        #[::zyris::async_trait]
        #rewritten
    }
}

fn request_struct(vis: &syn::Visibility, method: &ToolMethod) -> TokenStream2 {
    let ident = &method.request_struct;
    let fields = method.args.iter().map(|(name, ty)| quote! { pub #name: #ty });
    quote! {
        #[derive(
            ::zyris::serde::Serialize,
            ::zyris::serde::Deserialize,
            ::zyris::schemars::JsonSchema,
        )]
        #[serde(crate = "zyris::serde")]
        #[schemars(crate = "zyris::schemars")]
        #vis struct #ident {
            #(#fields,)*
        }
    }
}

fn descriptor_fn(
    vis: &syn::Visibility,
    fn_ident: &Ident,
    cap_name: &str,
    version: u32,
    methods: &[ToolMethod],
) -> TokenStream2 {
    let tools = methods.iter().map(|method| {
        let tool_name = method.ident.to_string();
        let description = &method.description;
        let request = &method.request_struct;
        let response = &method.response;
        let transfer = match method.transfer {
            Transfer::Unary => quote!(::zyris::Transfer::Unary),
            Transfer::UniStream => quote!(::zyris::Transfer::UniStream),
        };
        let item_schema = match &method.stream_item {
            Some(item) => quote! {
                Some(::zyris::serde_json::to_value(
                    ::zyris::schemars::schema_for!(#item)
                ).expect("item schema"))
            },
            None => quote!(None),
        };
        quote! {
            ::zyris::ToolDescriptor {
                name: #tool_name.to_string(),
                description: #description.to_string(),
                transfer: #transfer,
                request_schema: ::zyris::serde_json::to_value(
                    ::zyris::schemars::schema_for!(#request)
                ).expect("request schema"),
                response_schema: Some(::zyris::serde_json::to_value(
                    ::zyris::schemars::schema_for!(#response)
                ).expect("response schema")),
                item_schema: #item_schema,
            }
        }
    });
    quote! {
        #vis fn #fn_ident() -> ::zyris::CapabilityDescriptor {
            ::zyris::CapabilityDescriptor {
                name: #cap_name.to_string(),
                version: #version,
                tools: vec![#(#tools),*],
            }
        }
    }
}

fn server_impl(
    vis: &syn::Visibility,
    trait_ident: &Ident,
    descriptor_fn: &Ident,
    cap_name: &str,
    methods: &[ToolMethod],
) -> TokenStream2 {
    let server_ident = format_ident!("{}Server", trait_ident);
    let arms = methods.iter().map(|method| {
        let tool_name = method.ident.to_string();
        let request = &method.request_struct;
        let method_ident = &method.ident;
        let arg_names = method.args.iter().map(|(name, _)| name);
        match method.transfer {
            Transfer::Unary => quote! {
                #tool_name => {
                    let req: #request = ::zyris::decode_call(&call)?;
                    let out = self.0.#method_ident(#(req.#arg_names),*).await?;
                    ::zyris::encode_response(&out)
                }
            },
            Transfer::UniStream => quote! {
                #tool_name => {
                    let req: #request = ::zyris::decode_call(&call)?;
                    let serialization = call.serialization;
                    let streaming = self.0.#method_ident(#(req.#arg_names),*).await?;
                    ::zyris::encode_streaming(streaming, serialization)
                }
            },
        }
    });
    quote! {
        #vis struct #server_ident<T: #trait_ident>(pub T);

        #[::zyris::async_trait]
        impl<T: #trait_ident> ::zyris::ServeCapability for #server_ident<T> {
            fn descriptor(&self) -> ::zyris::CapabilityDescriptor {
                #descriptor_fn()
            }

            async fn dispatch(
                &self,
                call: ::zyris::IncomingCall,
            ) -> ::zyris::Result<::zyris::Outgoing> {
                match call.tool.as_str() {
                    #(#arms)*
                    other => Err(::zyris::unknown_tool(#cap_name, other)),
                }
            }
        }
    }
}

fn client_impl(
    vis: &syn::Visibility,
    trait_ident: &Ident,
    cap_name: &str,
    version: u32,
    methods: &[ToolMethod],
) -> TokenStream2 {
    let client_ident = format_ident!("{}Client", trait_ident);
    let trait_methods = methods.iter().map(|method| {
        let tool_name = method.ident.to_string();
        let method_ident = &method.ident;
        let request = &method.request_struct;
        let params = method.args.iter().map(|(name, ty)| quote! { #name: #ty });
        let arg_names: Vec<_> = method.args.iter().map(|(name, _)| name).collect();
        let response = &method.response;
        match method.transfer {
            Transfer::Unary => quote! {
                async fn #method_ident(&self, #(#params),*) -> ::zyris::Result<#response> {
                    self.handle.call(#tool_name, &#request { #(#arg_names),* }).await
                }
            },
            Transfer::UniStream => {
                let item = method.stream_item.as_ref().unwrap();
                quote! {
                    async fn #method_ident(
                        &self,
                        #(#params),*
                    ) -> ::zyris::Result<::zyris::Streaming<#response, #item>> {
                        self.handle
                            .call_streaming(#tool_name, &#request { #(#arg_names),* })
                            .await
                    }
                }
            }
        }
    });
    quote! {
        #vis struct #client_ident {
            handle: ::zyris::CapabilityHandle,
        }

        impl #client_ident {
            pub fn handle(&self) -> &::zyris::CapabilityHandle {
                &self.handle
            }
        }

        impl ::zyris::CapabilityClient for #client_ident {
            const NAME: &'static str = #cap_name;
            const VERSION: u32 = #version;

            fn from_handle(handle: ::zyris::CapabilityHandle) -> Self {
                Self { handle }
            }
        }

        #[::zyris::async_trait]
        impl #trait_ident for #client_ident {
            #(#trait_methods)*
        }
    }
}

fn snake_case(input: &str) -> String {
    let mut out = String::new();
    for (i, ch) in input.chars().enumerate() {
        if ch.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

fn pascal_case(input: &str) -> String {
    input
        .split('_')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect()
}
