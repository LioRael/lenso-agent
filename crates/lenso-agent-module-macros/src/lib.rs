//! Product authoring macros for Agent Modules.

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{Expr, FnArg, ItemFn, LitStr, PatType, Token, parse_macro_input};

struct ToolAttributes {
    name: Expr,
    description: LitStr,
}

impl syn::parse::Parse for ToolAttributes {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let mut name = None;
        let mut description = None;
        while !input.is_empty() {
            let key: syn::Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            match key.to_string().as_str() {
                "name" => name = Some(input.parse()?),
                "description" => description = Some(input.parse()?),
                _ => {
                    return Err(syn::Error::new(
                        key.span(),
                        "expected `name` or `description`",
                    ));
                }
            }
            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }
        Ok(Self {
            name: name.ok_or_else(|| input.error("missing `name`"))?,
            description: description.ok_or_else(|| input.error("missing `description`"))?,
        })
    }
}

/// Derives one stateless Agent Tool Provider Module from a typed function.
#[proc_macro_attribute]
pub fn tool(attributes: TokenStream, item: TokenStream) -> TokenStream {
    let attributes = parse_macro_input!(attributes as ToolAttributes);
    let function = parse_macro_input!(item as ItemFn);
    expand_tool(&attributes, &function)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand_tool(
    attributes: &ToolAttributes,
    function: &ItemFn,
) -> syn::Result<proc_macro2::TokenStream> {
    let [
        FnArg::Typed(PatType {
            ty: arguments_type, ..
        }),
    ] = function.sig.inputs.iter().collect::<Vec<_>>().as_slice()
    else {
        return Err(syn::Error::new_spanned(
            &function.sig.inputs,
            "an Agent Tool must accept exactly one typed arguments value",
        ));
    };

    let function_name = &function.sig.ident;
    let provider = format_ident!("__LensoToolProvider_{function_name}");
    let instantiate = format_ident!("__lenso_instantiate_{function_name}");
    let name = &attributes.name;
    let description = &attributes.description;
    let invoke = if function.sig.asyncness.is_some() {
        quote!(#function_name(arguments).await)
    } else {
        quote!(#function_name(arguments))
    };

    Ok(quote! {
        #function

        #[doc(hidden)]
        #[derive(Clone, Copy, Debug)]
        struct #provider;

        impl ::lenso_agent_module::__private::ToolProviderProvider for #provider {
            fn catalog(
                &self,
                _context: ::lenso_agent_module::__private::InvocationContext,
                _request: ::lenso_agent_module::__private::CatalogRequest,
            ) -> ::lenso_agent_module::__private::LocalBoxFuture<'static, Result<Result<
                ::lenso_agent_module::__private::CatalogResponse,
                ::lenso_agent_module::__private::CatalogError,
            >, ::lenso_agent_module::__private::RuntimeFailure>> {
                static INPUT_SCHEMA: std::sync::OnceLock<String> = std::sync::OnceLock::new();
                let schema = INPUT_SCHEMA.get_or_init(|| {
                    let schema = ::lenso_agent_module::__private::schema_for!(#arguments_type);
                    ::lenso_agent_module::__private::serde_json::to_string(&schema)
                        .expect("derived Tool input Schema must serialize")
                }).clone();
                Box::pin(::lenso_agent_module::__private::ready(Ok(Ok(
                    ::lenso_agent_module::__private::CatalogResponse {
                        tools: vec![::lenso_agent_module::__private::CatalogResponseToolsItem {
                            name: (#name).to_owned(),
                            description: #description.to_owned(),
                            input_schema_json: schema,
                        }],
                    },
                ))))
            }

            fn execute(
                &self,
                _context: ::lenso_agent_module::__private::InvocationContext,
                request: ::lenso_agent_module::__private::ExecuteRequest,
            ) -> ::lenso_agent_module::__private::LocalBoxFuture<'static, Result<Result<
                ::lenso_agent_module::__private::ExecuteResponse,
                ::lenso_agent_module::__private::ExecuteError,
            >, ::lenso_agent_module::__private::RuntimeFailure>> {
                Box::pin(async move {
                    if request.name != #name {
                        return Ok(Err(::lenso_agent_module::__private::ExecuteError::NotFound));
                    }
                    let arguments = match ::lenso_agent_module::__private::serde_json::from_str::<#arguments_type>(&request.arguments_json) {
                        Ok(arguments) => arguments,
                        Err(_) => return Ok(Err(::lenso_agent_module::__private::ExecuteError::InvalidArguments)),
                    };
                    Ok(#invoke)
                })
            }
        }

        #[lenso_native_adapter::module]
        fn #instantiate(
            context: ::lenso_native_adapter::NativeModuleFactoryContext<'_>,
        ) -> Result<
            ::lenso_native_adapter::NativeModuleInstance,
            ::lenso_native_adapter::RuntimeFailure,
        > {
            if context.entrypoint() != "default" || context.configuration() != "{}" {
                return Err(::lenso_native_adapter::RuntimeFailure::InvalidResolvedPlan {
                    detail: format!(
                        "{} requires the default entrypoint and empty configuration",
                        #name,
                    ),
                });
            }
            let endpoint = std::rc::Rc::new(
                ::lenso_agent_module::__private::ToolProviderEndpoint::new(#provider),
            ) as std::rc::Rc<dyn ::lenso_agent_module::__private::NativeRequestEndpoint>;
            Ok(::lenso_native_adapter::NativeModuleInstance::new(vec![endpoint]))
        }
    })
}
