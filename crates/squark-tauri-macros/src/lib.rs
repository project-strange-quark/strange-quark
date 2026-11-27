// SPDX-License-Identifier: MIT
//! `#[squark_tauri::command]` and `squark_tauri::generate_handler!`.
//!
//! `#[command]` keeps a Tauri command as written and adds a hidden wrapper,
//! `__tauri_command_<name>`, exported with `#[squark_native::export]` under
//! the command's own name. The wrapper takes the command's JSON arguments
//! unchanged and builds its Tauri-typed parameters (`AppHandle`,
//! `State<'_, T>`, `Window`, `WebviewWindow`) from the native app handle.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{
    FnArg, GenericArgument, Ident, ItemFn, LitStr, Meta, Pat, Path, PathArguments, Token, Type,
    parse::Parser, parse_macro_input, punctuated::Punctuated, spanned::Spanned,
};

fn last_segment(ty: &Type) -> Option<&syn::PathSegment> {
    match ty {
        Type::Path(path) => path.path.segments.last(),
        Type::Group(group) => last_segment(&group.elem),
        Type::Paren(paren) => last_segment(&paren.elem),
        _ => None,
    }
}

fn state_type(segment: &syn::PathSegment) -> Option<Type> {
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    args.args.iter().find_map(|arg| match arg {
        GenericArgument::Type(ty) => Some(ty.clone()),
        _ => None,
    })
}

fn expand(attr: TokenStream2, function: ItemFn) -> syn::Result<TokenStream2> {
    // `rename_all` passes through to squark_native::export.
    let options = Punctuated::<Meta, Token![,]>::parse_terminated.parse2(attr)?;
    let name = &function.sig.ident;
    let wrapper = format_ident!("__tauri_command_{}", name);
    let command = LitStr::new(&name.to_string(), name.span());
    let vis = &function.vis;
    let asyncness = &function.sig.asyncness;
    let output = &function.sig.output;
    let mut params = vec![quote!(__squark_app: ::squark_native::App)];
    let mut args = Vec::new();
    for input in &function.sig.inputs {
        let FnArg::Typed(typed) = input else {
            return Err(syn::Error::new(input.span(), "commands cannot take self"));
        };
        let Pat::Ident(pat) = &*typed.pat else {
            return Err(syn::Error::new(
                typed.pat.span(),
                "command parameters must be plain names",
            ));
        };
        let ident = &pat.ident;
        let ty = &typed.ty;
        match last_segment(ty) {
            Some(segment) if segment.ident == "AppHandle" => {
                args.push(quote!(::squark_tauri::AppHandle::from_native(
                    ::core::clone::Clone::clone(&__squark_app)
                )));
            }
            Some(segment) if segment.ident == "Window" || segment.ident == "WebviewWindow" => {
                args.push(quote!(
                    ::squark_tauri::AppHandle::from_native(::core::clone::Clone::clone(
                        &__squark_app
                    ))
                    .main_window()
                ));
            }
            Some(segment) if segment.ident == "State" => {
                let inner = state_type(segment).ok_or_else(|| {
                    syn::Error::new(segment.span(), "State needs a type: State<'_, T>")
                })?;
                args.push(
                    quote!(::squark_tauri::State::<#inner>::__from_app(&__squark_app, #command)),
                );
            }
            _ => {
                params.push(quote!(#ident: #ty));
                args.push(quote!(#ident));
            }
        }
    }
    let call = if asyncness.is_some() {
        quote!(#name(#(#args),*).await)
    } else {
        quote!(#name(#(#args),*))
    };
    let options = options.iter();
    Ok(quote! {
        #function

        #[doc(hidden)]
        #[allow(non_snake_case, clippy::too_many_arguments)]
        #[::squark_native::export(rename = #command #(, #options)*)]
        #vis #asyncness fn #wrapper(#(#params),*) #output {
            #call
        }
    })
}

/// A Tauri command, also exported as a Strange Quark native command.
#[proc_macro_attribute]
pub fn command(attr: TokenStream, item: TokenStream) -> TokenStream {
    let function = parse_macro_input!(item as ItemFn);
    expand(attr.into(), function)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// `generate_handler![a, module::b]`: the commands for
/// `squark_native::Module::commands`, as Tauri's macro of the same name
/// lists them for `Builder::invoke_handler`.
#[proc_macro]
pub fn generate_handler(input: TokenStream) -> TokenStream {
    let paths = match Punctuated::<Path, Token![,]>::parse_terminated.parse(input) {
        Ok(paths) => paths,
        Err(error) => return error.into_compile_error().into(),
    };
    let markers = paths.into_iter().map(|mut path| {
        if let Some(last) = path.segments.last_mut() {
            last.ident = Ident::new(
                &format!("__squark_command___tauri_command_{}", last.ident),
                last.ident.span(),
            );
        }
        quote!(::std::boxed::Box::new(#path) as ::std::boxed::Box<dyn ::squark_native::Command>)
    });
    quote!(::std::vec![#(#markers),*]).into()
}
