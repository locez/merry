//! Attribute macros for declaring typed Merry tools.
//!
//! The public SDK re-exports these macros from `merry`; this crate contains
//! the procedural-macro implementation required by Rust's crate model.

use proc_macro::TokenStream;
use proc_macro_crate::{FoundCrate, crate_name};
use quote::{format_ident, quote};
use syn::{
    Error, FnArg, Ident, Item, ItemFn, ItemStruct, LitStr, Path, Result, Token,
    ext::IdentExt,
    parse::{Parse, ParseStream},
    parse_macro_input,
};

struct ToolArguments {
    name: Option<LitStr>,
    description: LitStr,
    crate_path: Option<Path>,
}

impl Parse for ToolArguments {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let mut name = None;
        let mut description = None;
        let mut crate_path = None;

        while !input.is_empty() {
            let key = input.call(Ident::parse_any)?;
            input.parse::<Token![=]>()?;
            let value: LitStr = input.parse()?;

            match key.to_string().as_str() {
                "name" => {
                    if name.replace(value).is_some() {
                        return Err(Error::new(key.span(), "duplicate `name` argument"));
                    }
                }
                "description" => {
                    if description.replace(value).is_some() {
                        return Err(Error::new(key.span(), "duplicate `description` argument"));
                    }
                }
                "crate" => {
                    if crate_path.replace(value.parse::<Path>()?).is_some() {
                        return Err(Error::new(key.span(), "duplicate `crate` argument"));
                    }
                }
                _ => {
                    return Err(Error::new(
                        key.span(),
                        "expected `name`, `description`, or `crate`",
                    ));
                }
            }

            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }

        let description = description.ok_or_else(|| {
            Error::new(
                proc_macro2::Span::call_site(),
                "missing required `description` argument",
            )
        })?;

        Ok(Self {
            name,
            description,
            crate_path,
        })
    }
}

/// Declares a typed Merry tool handler or input definition.
///
/// On an async function with one input argument, this generates a
/// `<function>_tool()` factory with the handler's visibility. On a struct, it generates an inherent
/// `tool_spec()` factory using the struct's `Deserialize` and `JsonSchema`
/// implementations. Runtime-owned executors can use the struct form while
/// retaining custom policy, tracing, and cancellation behavior.
///
/// Renamed `merry` dependencies are resolved automatically. Use
/// `crate = "crate"` to select a local re-export boundary explicitly.
/// The Rust type checker validates handler return types, including aliases.
#[proc_macro_attribute]
pub fn tool(attributes: TokenStream, item: TokenStream) -> TokenStream {
    let arguments = parse_macro_input!(attributes as ToolArguments);
    let item = parse_macro_input!(item as Item);

    let expansion = match item {
        Item::Fn(function) => expand_handler(arguments, function),
        Item::Struct(structure) => expand_definition(arguments, structure),
        other => Err(Error::new_spanned(
            other,
            "`#[tool]` supports async functions and structs only",
        )),
    };

    match expansion {
        Ok(expanded) => expanded.into(),
        Err(error) => error.into_compile_error().into(),
    }
}

fn expand_handler(arguments: ToolArguments, function: ItemFn) -> Result<proc_macro2::TokenStream> {
    if function.sig.asyncness.is_none() {
        return Err(Error::new_spanned(
            function.sig.fn_token,
            "`#[tool]` requires an async function",
        ));
    }

    if !function.sig.generics.params.is_empty() || function.sig.generics.where_clause.is_some() {
        return Err(Error::new_spanned(
            &function.sig.generics,
            "`#[tool]` does not support generic functions",
        ));
    }

    if function.sig.inputs.len() != 1 {
        return Err(Error::new_spanned(
            &function.sig.inputs,
            "`#[tool]` requires exactly one typed input argument",
        ));
    }

    if !matches!(function.sig.inputs.first(), Some(FnArg::Typed(_))) {
        return Err(Error::new_spanned(
            &function.sig.inputs,
            "`#[tool]` requires a typed input argument",
        ));
    }

    let function_name = &function.sig.ident;
    let visibility = &function.vis;
    let generated_name = format_ident!("{}_tool", function_name);
    let tool_name = arguments
        .name
        .unwrap_or_else(|| LitStr::new(&function_name.unraw().to_string(), function_name.span()));
    let description = arguments.description;
    let crate_path = resolve_crate_path(arguments.crate_path)?;

    Ok(quote! {
        #function

        /// Builds the typed Merry tool for this handler.
        #visibility fn #generated_name() -> ::core::result::Result<#crate_path::Tool, #crate_path::ToolBuildError> {
            #crate_path::Tool::new(#tool_name, #description, #function_name)
        }
    })
}

fn expand_definition(
    arguments: ToolArguments,
    structure: ItemStruct,
) -> Result<proc_macro2::TokenStream> {
    if !structure.generics.params.is_empty() || structure.generics.where_clause.is_some() {
        return Err(Error::new_spanned(
            &structure.generics,
            "`#[tool]` does not support generic input definitions",
        ));
    }

    let structure_name = &structure.ident;
    let visibility = &structure.vis;
    let tool_name = arguments
        .name
        .unwrap_or_else(|| LitStr::new(&structure_name.to_string(), structure_name.span()));
    let description = arguments.description;
    let crate_path = resolve_crate_path(arguments.crate_path)?;

    Ok(quote! {
        #structure

        impl #structure_name {
            /// Builds the provider-neutral tool specification for this input.
            #visibility fn tool_spec() -> ::core::result::Result<#crate_path::ToolSpec, #crate_path::ToolBuildError> {
                #crate_path::Tool::spec_for::<Self>(#tool_name, #description)
            }
        }
    })
}

fn resolve_crate_path(explicit: Option<Path>) -> Result<Path> {
    if let Some(path) = explicit {
        return Ok(path);
    }
    match crate_name("merry") {
        Ok(FoundCrate::Itself) => Ok(syn::parse_quote!(::merry)),
        Ok(FoundCrate::Name(name)) => {
            let identifier = Ident::new(&name, proc_macro2::Span::call_site());
            Ok(syn::parse_quote!(::#identifier))
        }
        Err(error) => Err(Error::new(
            proc_macro2::Span::call_site(),
            format!(
                "could not resolve the merry dependency: {error}; specify `crate = \"path\"` for an explicit tool API boundary"
            ),
        )),
    }
}

#[cfg(test)]
mod tests;
