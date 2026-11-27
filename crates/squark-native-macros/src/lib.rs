// SPDX-License-Identifier: MIT
//! `#[squark_native::export]` and `squark_native::handlers!`.
//!
//! `#[export]` keeps the function as written and adds a hidden unit struct,
//! `__squark_command_<name>`, implementing `squark_native::Command`. Its
//! arguments arrive as one JSON object whose keys are the parameter names in
//! camelCase. Parameters typed `App` receive the app handle and `State<T>`
//! the managed `T`; neither comes from JSON.
//! `handlers![a, module::b]` lists the structs for `Module::commands`.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{
    Expr, ExprLit, FnArg, GenericArgument, Ident, ItemFn, Lit, LitStr, Meta, Pat, Path,
    PathArguments, ReturnType, Token, Type, parse::Parser, parse_macro_input,
    punctuated::Punctuated, spanned::Spanned,
};

enum Param {
    App,
    State(Type),
    Json { name: Ident, ty: Type },
}

/// The last path segment's identifier of a type, ignoring references.
fn last_segment(ty: &Type) -> Option<&syn::PathSegment> {
    match ty {
        Type::Path(path) => path.path.segments.last(),
        Type::Group(group) => last_segment(&group.elem),
        Type::Paren(paren) => last_segment(&paren.elem),
        _ => None,
    }
}

fn classify(arg: &FnArg) -> syn::Result<Param> {
    let FnArg::Typed(typed) = arg else {
        return Err(syn::Error::new(
            arg.span(),
            "exported commands are free functions; `self` is not supported",
        ));
    };
    let Pat::Ident(pat) = &*typed.pat else {
        return Err(syn::Error::new(
            typed.pat.span(),
            "exported command parameters must be plain names",
        ));
    };
    if let Type::Reference(reference) = &*typed.ty {
        return Err(syn::Error::new(
            reference.span(),
            "exported command parameters must be owned (use String, not &str)",
        ));
    }
    match last_segment(&typed.ty) {
        Some(segment) if segment.ident == "App" && segment.arguments.is_empty() => Ok(Param::App),
        Some(segment) if segment.ident == "State" => {
            let PathArguments::AngleBracketed(args) = &segment.arguments else {
                return Err(syn::Error::new(
                    segment.span(),
                    "State needs a type: State<T>",
                ));
            };
            let inner = args
                .args
                .iter()
                .find_map(|arg| match arg {
                    GenericArgument::Type(ty) => Some(ty.clone()),
                    _ => None,
                })
                .ok_or_else(|| syn::Error::new(segment.span(), "State needs a type: State<T>"))?;
            Ok(Param::State(inner))
        }
        _ => Ok(Param::Json {
            name: pat.ident.clone(),
            ty: (*typed.ty).clone(),
        }),
    }
}

fn camel_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut upper = false;
    for (index, ch) in name.trim_start_matches('_').chars().enumerate() {
        if ch == '_' {
            upper = index > 0;
        } else if upper {
            out.extend(ch.to_uppercase());
            upper = false;
        } else {
            out.push(ch);
        }
    }
    out
}

struct Options {
    rename: Option<String>,
    rename_all: RenameAll,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RenameAll {
    Camel,
    Snake,
}

fn options(attr: TokenStream2) -> syn::Result<Options> {
    let mut options = Options {
        rename: None,
        rename_all: RenameAll::Camel,
    };
    let metas = Punctuated::<Meta, Token![,]>::parse_terminated.parse2(attr)?;
    for meta in metas {
        let Meta::NameValue(pair) = &meta else {
            return Err(syn::Error::new(meta.span(), "expected `name = \"value\"`"));
        };
        let Expr::Lit(ExprLit {
            lit: Lit::Str(value),
            ..
        }) = &pair.value
        else {
            return Err(syn::Error::new(pair.value.span(), "expected a string"));
        };
        if pair.path.is_ident("rename") {
            options.rename = Some(value.value());
        } else if pair.path.is_ident("rename_all") {
            options.rename_all = match value.value().as_str() {
                "camelCase" => RenameAll::Camel,
                "snake_case" => RenameAll::Snake,
                other => {
                    return Err(syn::Error::new(
                        value.span(),
                        format!(
                            "rename_all must be \"camelCase\" or \"snake_case\", not {other:?}"
                        ),
                    ));
                }
            };
        } else {
            return Err(syn::Error::new(
                pair.path.span(),
                "unknown option; expected `rename` or `rename_all`",
            ));
        }
    }
    Ok(options)
}

/// Whether a return type is spelled `Result<..>` (any path ending in `Result`).
fn returns_result(output: &ReturnType) -> bool {
    match output {
        ReturnType::Default => false,
        ReturnType::Type(_, ty) => {
            last_segment(ty).is_some_and(|segment| segment.ident == "Result")
        }
    }
}

fn expand(attr: TokenStream2, function: ItemFn) -> syn::Result<TokenStream2> {
    let options = options(attr)?;
    if !function.sig.generics.params.is_empty() {
        return Err(syn::Error::new(
            function.sig.generics.span(),
            "exported commands cannot be generic",
        ));
    }
    let name = &function.sig.ident;
    let command = options.rename.unwrap_or_else(|| name.to_string());
    let command = LitStr::new(&command, name.span());
    let marker = format_ident!("__squark_command_{}", name);
    let vis = &function.vis;
    let params = function
        .sig
        .inputs
        .iter()
        .map(classify)
        .collect::<syn::Result<Vec<_>>>()?;

    let mut fields = Vec::new();
    let mut bindings = Vec::new();
    let mut call_args = Vec::new();
    for (index, param) in params.iter().enumerate() {
        match param {
            Param::App => call_args.push(quote!(::core::clone::Clone::clone(&app))),
            Param::State(inner) => {
                let local = format_ident!("__state{}", index);
                bindings.push(quote! {
                    let #local = app.state::<#inner>().ok_or_else(|| {
                        ::squark_native::__private::missing_state(#command, ::core::any::type_name::<#inner>())
                    })?;
                });
                call_args.push(quote!(#local));
            }
            Param::Json { name, ty } => {
                let key = match options.rename_all {
                    RenameAll::Camel => camel_case(&name.to_string()),
                    RenameAll::Snake => name.to_string(),
                };
                let key = LitStr::new(&key, name.span());
                fields.push(quote! {
                    #[serde(rename = #key)]
                    #name: #ty
                });
                call_args.push(quote!(__args.#name));
            }
        }
    }

    let invoke = quote!(#name(#(#call_args),*));
    let finish = if returns_result(&function.sig.output) {
        quote!(::squark_native::__private::result(__value))
    } else {
        quote!(::squark_native::__private::ok(&__value))
    };
    let parse = quote! {
        #[derive(::squark_native::__private::serde::Deserialize)]
        #[serde(crate = "::squark_native::__private::serde")]
        #[allow(non_camel_case_types, dead_code)]
        struct __Args { #(#fields,)* }
        let __args: __Args = ::squark_native::__private::parse_args(#command, &args)?;
    };
    let body = if function.sig.asyncness.is_some() {
        quote! {
            ::squark_native::Invocation::Async(::std::boxed::Box::pin(async move {
                #parse
                #(#bindings)*
                let __value = #invoke.await;
                #finish
            }))
        }
    } else {
        quote! {
            ::squark_native::Invocation::Blocking(::std::boxed::Box::new(move || {
                #parse
                #(#bindings)*
                let __value = #invoke;
                #finish
            }))
        }
    };
    Ok(quote! {
        #function

        #[doc(hidden)]
        #[allow(non_camel_case_types)]
        #vis struct #marker;

        impl ::squark_native::Command for #marker {
            fn name(&self) -> &'static str {
                #command
            }

            #[allow(unused_variables)]
            fn invoke(&self, app: ::squark_native::App, args: ::std::string::String) -> ::squark_native::Invocation {
                #body
            }
        }
    })
}

/// Export a function as a native command. See the crate documentation.
#[proc_macro_attribute]
pub fn export(attr: TokenStream, item: TokenStream) -> TokenStream {
    let function = parse_macro_input!(item as ItemFn);
    expand(attr.into(), function)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// `handlers![a, module::b]`: the commands exported with `#[export]`, for
/// `Module::commands`.
#[proc_macro]
pub fn handlers(input: TokenStream) -> TokenStream {
    let paths = match Punctuated::<Path, Token![,]>::parse_terminated.parse(input) {
        Ok(paths) => paths,
        Err(error) => return error.into_compile_error().into(),
    };
    let markers = paths.into_iter().map(|mut path| {
        if let Some(last) = path.segments.last_mut() {
            last.ident = Ident::new(
                &format!("__squark_command_{}", last.ident),
                last.ident.span(),
            );
        }
        quote!(::std::boxed::Box::new(#path) as ::std::boxed::Box<dyn ::squark_native::Command>)
    });
    quote!(::std::vec![#(#markers),*]).into()
}

#[cfg(test)]
mod tests {
    use super::camel_case;

    #[test]
    fn parameter_names_become_camel_case_keys() {
        assert_eq!(camel_case("session_id"), "sessionId");
        assert_eq!(camel_case("path"), "path");
        assert_eq!(camel_case("_unused_arg"), "unusedArg");
        assert_eq!(camel_case("a_b_c"), "aBC");
    }
}
