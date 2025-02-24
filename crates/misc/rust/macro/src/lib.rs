extern crate proc_macro;

use proc_macro2::TokenStream;
use quote::quote;
use syn::spanned::Spanned;

#[proc_macro_attribute]
pub fn wasmtime(
    _attr: proc_macro::TokenStream,
    item: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    let item = syn::parse_macro_input!(item as syn::ItemTrait);
    expand(item).unwrap_or_else(|e| e.to_compile_error()).into()
}

fn expand(item: syn::ItemTrait) -> syn::Result<TokenStream> {
    let definition = generate_struct(&item)?;
    let load = generate_load(&item)?;
    let methods = generate_methods(&item)?;
    let name = &item.ident;

    Ok(quote! {
        #definition
        impl #name {
            #load
            #methods
        }
    })
}

fn generate_struct(item: &syn::ItemTrait) -> syn::Result<TokenStream> {
    let vis = &item.vis;
    let name = &item.ident;
    let root = root();
    Ok(quote! {
        #vis struct #name {
            instance: #root::wasmtime::HostRef<#root::wasmtime::Instance>,
            data: #root::wasmtime_interface_types::ModuleData,
        }
    })
}

fn generate_load(item: &syn::ItemTrait) -> syn::Result<TokenStream> {
    let vis = &item.vis;
    let name = &item.ident;
    let root = root();
    Ok(quote! {
        #vis fn load_file(path: impl AsRef<std::path::Path>) -> #root::anyhow::Result<#name> {
            let bytes = std::fs::read(path)?;

            use #root::wasmtime::{HostRef, Config, Extern, Engine, Store, Instance, Module};
            use #root::anyhow::{bail, format_err};

            let mut config = Config::new();
            config.features(#root::wasmtime_jit::Features {
                multi_value: true,
                ..Default::default()
            });
            let engine = HostRef::new(Engine::new(&config));
            let store = HostRef::new(Store::new(&engine));
            let global_exports = store.borrow().global_exports().clone();

            let data = #root::wasmtime_interface_types::ModuleData::new(&bytes)?;

            let module = HostRef::new(Module::new(&store, &bytes)?);

            let mut imports: Vec<Extern> = Vec::new();
            if let Some(module_name) = data.find_wasi_module_name() {
                let wasi_instance = #root::wasmtime_wasi::create_wasi_instance(&store, &[], &[], &[])
                    .map_err(|e| format_err!("wasm instantiation error: {:?}", e))?;
                for i in module.borrow().imports().iter() {
                    if i.module().as_str() != module_name {
                        bail!("unknown import module {}", i.module().as_str());
                    }
                    if let Some(export) = wasi_instance.find_export_by_name(i.name().as_str()) {
                        imports.push(export.clone());
                    } else {
                        bail!("unknown import {}:{}", i.module().as_str(), i.name().as_str())
                    }
                }
            }
            let instance = HostRef::new(
                Instance::new(&store, &module, &imports).map_err(|t| format_err!("instantiation trap: {:?}", t))?
            );

            Ok(#name { instance, data })
        }
    })
}

fn generate_methods(item: &syn::ItemTrait) -> syn::Result<TokenStream> {
    macro_rules! bail {
        ($e:expr, $($fmt:tt)*) => (
            return Err(syn::Error::new($e.span(), format!($($fmt)*)));
        )
    }
    let mut result = TokenStream::new();
    let root = root();
    let vis = &item.vis;

    for item in item.items.iter() {
        let method = match item {
            syn::TraitItem::Method(f) => f,
            other => bail!(other, "only methods are allowed"),
        };
        if let Some(e) = &method.default {
            bail!(e, "cannot specify an implementation of methods");
        }
        if let Some(t) = &method.sig.constness {
            bail!(t, "cannot be `const`");
        }
        if let Some(t) = &method.sig.asyncness {
            bail!(t, "cannot be `async`");
        }
        match &method.sig.inputs.first() {
            Some(syn::FnArg::Receiver(_)) => {}
            Some(t) => bail!(t, "first arugment needs to be \"self\""),
            None => bail!(
                method.sig,
                "trait method requires at least one argument which needs to be \"self\""
            ),
        }

        let mut args = Vec::new();
        for arg in method.sig.inputs.iter() {
            let arg = match arg {
                syn::FnArg::Receiver(_) => continue,
                syn::FnArg::Typed(arg) => arg,
            };
            let ident = match &*arg.pat {
                syn::Pat::Ident(i) => i,
                other => bail!(other, "must use bare idents for arguments"),
            };
            if let Some(t) = &ident.by_ref {
                bail!(t, "arguments cannot bind by reference");
            }
            if let Some(t) = &ident.mutability {
                bail!(t, "arguments cannot be mutable");
            }
            if let Some((_, t)) = &ident.subpat {
                bail!(t, "arguments cannot have sub-bindings");
            }
            let ident = &ident.ident;
            args.push(quote! {
                #root::wasmtime_interface_types::Value::from(#ident)
            });
        }

        let convert_ret = match &method.sig.output {
            syn::ReturnType::Default => {
                quote! {
                    <() as #root::FromVecValue>::from(results)
                }
            }
            syn::ReturnType::Type(_, ty) => match &**ty {
                syn::Type::Tuple(..) => {
                    quote! { <#ty as #root::FromVecValue>::from(results) }
                }
                _ => {
                    quote! { <(#ty,) as #root::FromVecValue>::from(results).map(|t| t.0) }
                }
            },
        };

        let sig = &method.sig;
        let attrs = &method.attrs;
        let name = &method.sig.ident;

        result.extend(quote! {
            #(#attrs)*
            #vis #sig {
                let args = [
                    #(#args),*
                ];
                let results = self.data.invoke_export(
                    &self.instance,
                    stringify!(#name),
                    &args,
                ).expect("wasm execution failed");
                #convert_ret.expect("failed to convert return type")
            }
        });
    }

    Ok(result)
}

fn root() -> TokenStream {
    quote! { wasmtime_rust::__rt }
}
