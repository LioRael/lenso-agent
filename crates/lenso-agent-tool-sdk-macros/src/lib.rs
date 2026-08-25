//! Procedural macros for typed Agent Tool Provider authoring.

use proc_macro::TokenStream;
use quote::quote;
use syn::{
    Attribute, Expr, ExprLit, FnArg, ImplItem, ImplItemFn, ItemImpl, Lit, LitStr, MetaNameValue,
    ReturnType, Token, parse::Parser, punctuated::Punctuated, spanned::Spanned,
};

/// Derives one Tool Provider catalog and dispatcher from typed methods.
///
/// Each method marked with `#[tool(name = "...", description = "...")]` accepts exactly one typed
/// argument (optionally after `&self`) and returns the Tool Provider contract's
/// `Result<ExecuteResponse, ExecuteError>` shape. Both synchronous and asynchronous Tools are
/// supported.
#[proc_macro_attribute]
pub fn tool_provider(attribute: TokenStream, item: TokenStream) -> TokenStream {
    if !attribute.is_empty() {
        return syn::Error::new(
            proc_macro2::Span::call_site(),
            "tool_provider does not accept arguments",
        )
        .into_compile_error()
        .into();
    }
    let mut implementation = syn::parse_macro_input!(item as ItemImpl);
    expand(&mut implementation)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

struct ToolMethod {
    method: syn::Ident,
    argument_type: syn::Type,
    takes_self: bool,
    is_async: bool,
    name: LitStr,
    description: LitStr,
}

fn expand(implementation: &mut ItemImpl) -> syn::Result<proc_macro2::TokenStream> {
    validate_impl(implementation)?;
    let tools = collect_tools(implementation)?;
    let self_type = &implementation.self_ty;
    let catalog_entries = tools.iter().map(catalog_entry);
    let dispatch_arms = tools.iter().map(dispatch_arm);

    Ok(quote! {
        #implementation

        #[lenso::provides(tool_provider_contract::ToolProvider)]
        impl #self_type {
            async fn catalog(
                &self,
                _context: ::lenso::Ctx,
                _request: ::lenso_agent_tool_sdk::__private::contract::CatalogRequest,
            ) -> ::lenso::ModuleResult<
                ::lenso_agent_tool_sdk::__private::contract::CatalogResponse,
                ::lenso_agent_tool_sdk::__private::contract::CatalogError,
            > {
                Ok(::lenso_agent_tool_sdk::__private::contract::CatalogResponse {
                    tools: vec![#(#catalog_entries),*],
                })
            }

            async fn execute(
                &self,
                _context: ::lenso::Ctx,
                request: ::lenso_agent_tool_sdk::__private::contract::ExecuteRequest,
            ) -> ::lenso::ModuleResult<
                ::lenso_agent_tool_sdk::__private::contract::ExecuteResponse,
                ::lenso_agent_tool_sdk::__private::contract::ExecuteError,
            > {
                match request.name.as_str() {
                    #(#dispatch_arms,)*
                    _ => Err(::lenso::ModuleError::domain(
                        ::lenso_agent_tool_sdk::__private::contract::ExecuteError::NotFound,
                    )),
                }
            }
        }
    })
}

fn validate_impl(implementation: &ItemImpl) -> syn::Result<()> {
    if implementation.trait_.is_some() {
        return Err(syn::Error::new(
            implementation.impl_token.span,
            "tool_provider requires an inherent impl",
        ));
    }
    if !implementation.generics.params.is_empty() {
        return Err(syn::Error::new(
            implementation.generics.span(),
            "tool_provider does not support generic impl blocks",
        ));
    }

    if !matches!(&*implementation.self_ty, syn::Type::Path(_)) {
        return Err(syn::Error::new(
            implementation.self_ty.span(),
            "tool_provider requires a named Module type",
        ));
    }
    Ok(())
}

fn collect_tools(implementation: &mut ItemImpl) -> syn::Result<Vec<ToolMethod>> {
    let mut tools = Vec::new();
    let mut names = std::collections::BTreeSet::new();
    for item in &mut implementation.items {
        let ImplItem::Fn(method) = item else {
            continue;
        };
        let Some(tool) = parse_tool_method(method)? else {
            continue;
        };
        if !names.insert(tool.name.value()) {
            return Err(syn::Error::new(
                tool.name.span(),
                "duplicate Tool name in provider",
            ));
        }
        tools.push(tool);
    }
    if tools.is_empty() {
        return Err(syn::Error::new(
            implementation.self_ty.span(),
            "tool_provider requires at least one #[tool(...)] method",
        ));
    }
    Ok(tools)
}

fn parse_tool_method(method: &mut ImplItemFn) -> syn::Result<Option<ToolMethod>> {
    let Some(attribute_index) = method
        .attrs
        .iter()
        .position(|attribute| attribute.path().is_ident("tool"))
    else {
        return Ok(None);
    };
    let attribute = method.attrs.remove(attribute_index);
    let (name, description) = parse_tool_attribute(&attribute)?;
    if matches!(method.sig.output, ReturnType::Default) {
        return Err(syn::Error::new(
            method.sig.ident.span(),
            "Tool methods must return Result<ExecuteResponse, ExecuteError>",
        ));
    }
    let mut inputs = method.sig.inputs.iter();
    let first = inputs.next().ok_or_else(|| {
        syn::Error::new(
            method.sig.ident.span(),
            "Tool methods require one typed argument",
        )
    })?;
    let (takes_self, argument) = tool_argument(first, &mut inputs, method)?;
    if inputs.next().is_some() {
        return Err(syn::Error::new(
            method.sig.inputs.span(),
            "Tool methods accept exactly one typed argument",
        ));
    }
    Ok(Some(ToolMethod {
        method: method.sig.ident.clone(),
        argument_type: (*argument.ty).clone(),
        takes_self,
        is_async: method.sig.asyncness.is_some(),
        name,
        description,
    }))
}

fn tool_argument<'a>(
    first: &'a FnArg,
    inputs: &mut impl Iterator<Item = &'a FnArg>,
    method: &ImplItemFn,
) -> syn::Result<(bool, &'a syn::PatType)> {
    match first {
        FnArg::Receiver(receiver) => {
            if receiver.reference.is_none() || receiver.mutability.is_some() {
                return Err(syn::Error::new(
                    receiver.span(),
                    "Tool methods may receive only &self",
                ));
            }
            let Some(FnArg::Typed(argument)) = inputs.next() else {
                return Err(syn::Error::new(
                    method.sig.ident.span(),
                    "Tool methods require one typed argument after &self",
                ));
            };
            Ok((true, argument))
        }
        FnArg::Typed(argument) => Ok((false, argument)),
    }
}

fn catalog_entry(tool: &ToolMethod) -> proc_macro2::TokenStream {
    let argument_type = &tool.argument_type;
    let name = &tool.name;
    let description = &tool.description;
    quote! {
        ::lenso_agent_tool_sdk::__private::contract::ToolDefinition {
            name: #name.to_owned(),
            description: #description.to_owned(),
            input_schema_json: ::lenso_agent_tool_sdk::__private::serde_json::to_string(
                &::lenso_agent_tool_sdk::__private::schemars::schema_for!(#argument_type),
            )
            .expect("derived Tool input Schema must serialize")
            .try_into()
            .expect("derived Tool input Schema must be valid JSON"),
        }
    }
}

fn dispatch_arm(tool: &ToolMethod) -> proc_macro2::TokenStream {
    let method = &tool.method;
    let argument_type = &tool.argument_type;
    let name = &tool.name;
    let invoke = if tool.takes_self && tool.is_async {
        quote!(self.#method(arguments).await)
    } else if tool.takes_self {
        quote!(self.#method(arguments))
    } else if tool.is_async {
        quote!(Self::#method(arguments).await)
    } else {
        quote!(Self::#method(arguments))
    };
    quote! {
        #name => {
            let arguments: #argument_type =
                ::lenso_agent_tool_sdk::__private::serde_json::from_str(
                    request.arguments_json.as_str(),
                )
                .map_err(|_| ::lenso::ModuleError::domain(
                    ::lenso_agent_tool_sdk::__private::contract::ExecuteError::InvalidArguments,
                ))?;
            #invoke.map_err(::lenso::ModuleError::domain)
        }
    }
}

fn parse_tool_attribute(attribute: &Attribute) -> syn::Result<(LitStr, LitStr)> {
    let values = Punctuated::<MetaNameValue, Token![,]>::parse_terminated
        .parse2(attribute.meta.require_list()?.tokens.clone())?;
    let mut name = None;
    let mut description = None;
    for value in values {
        let literal = match value.value {
            Expr::Lit(ExprLit {
                lit: Lit::Str(literal),
                ..
            }) => literal,
            value => {
                return Err(syn::Error::new(
                    value.span(),
                    "Tool metadata values must be string literals",
                ));
            }
        };
        if value.path.is_ident("name") {
            if name.replace(literal).is_some() {
                return Err(syn::Error::new(value.path.span(), "duplicate Tool name"));
            }
        } else if value.path.is_ident("description") {
            if description.replace(literal).is_some() {
                return Err(syn::Error::new(
                    value.path.span(),
                    "duplicate Tool description",
                ));
            }
        } else {
            return Err(syn::Error::new(
                value.path.span(),
                "expected `name` or `description`",
            ));
        }
    }
    Ok((
        name.ok_or_else(|| syn::Error::new(attribute.span(), "missing Tool name"))?,
        description.ok_or_else(|| syn::Error::new(attribute.span(), "missing Tool description"))?,
    ))
}
