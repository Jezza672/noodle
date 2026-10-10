//! Derive macros for the node API.
//!
//! [`Ports`] turns one declaration of a node's ports into both its `Layout`
//! and the index constants its kernel reads inputs and outputs by, so the two
//! can't drift apart.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::punctuated::Punctuated;
use syn::{Attribute, DeriveInput, Expr, Ident, LitStr, Token, parse_macro_input};

/// Declares a node's ports on a struct with one `()` field per port.
///
/// ```ignore
/// #[derive(Ports)]
/// struct Ports {
///     #[input("in", "In")]
///     input: (),
///     #[param("gain", "Gain", ParamInfo::new(-60.0, 24.0, 0.0).unit(Unit::Decibels))]
///     gain: (),
///     #[output("out", "Out")]
///     out: (),
/// }
/// ```
///
/// This generates, on the struct:
///
/// - `Ports::layout() -> Layout`, a real-time layout with the ports in the
///   order the fields are written (`#[ports(offline)]` on the struct makes it
///   an offline layout, and `#[ports(nondeterministic)]` marks it so).
/// - An index constant per field, named after it in upper case (`Ports::GAIN`).
///   Audio and parameter inputs share one index space, in field order, as do
///   `output`, `event_input` and `event_output` ports among their own kind.
/// - `Ports::INPUTS`, `Ports::OUTPUTS`, `Ports::EVENT_INPUTS` and
///   `Ports::EVENT_OUTPUTS`, the port counts.
///
/// Field attributes: `#[input(key, name)]`, `#[param(key, name, ParamInfo)]`,
/// `#[output(key, name)]`, `#[event_input(key, name)]` and
/// `#[event_output(key, name)]`. Nodes whose ports depend on config, such as
/// Mix, build their `Layout` by hand instead.
#[proc_macro_derive(
    Ports,
    attributes(input, param, output, event_input, event_output, ports)
)]
pub fn derive_ports(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Input,
    Param,
    Output,
    EventInput,
    EventOutput,
}

impl Kind {
    fn of(attr: &Attribute) -> Option<Kind> {
        let path = attr.path();
        Some(if path.is_ident("input") {
            Kind::Input
        } else if path.is_ident("param") {
            Kind::Param
        } else if path.is_ident("output") {
            Kind::Output
        } else if path.is_ident("event_input") {
            Kind::EventInput
        } else if path.is_ident("event_output") {
            Kind::EventOutput
        } else {
            return None;
        })
    }

    /// Which index space the kind's ports are counted in.
    fn space(self) -> usize {
        match self {
            Kind::Input | Kind::Param => 0,
            Kind::Output => 1,
            Kind::EventInput => 2,
            Kind::EventOutput => 3,
        }
    }
}

struct Port {
    name: Ident,
    kind: Kind,
    key: LitStr,
    display: LitStr,
    info: Option<Expr>,
    index: usize,
}

fn expand(input: DeriveInput) -> syn::Result<TokenStream2> {
    let syn::Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            &input,
            "Ports can only be derived on a struct",
        ));
    };
    let syn::Fields::Named(fields) = &data.fields else {
        return Err(syn::Error::new_spanned(
            &input,
            "Ports needs a struct with named fields",
        ));
    };

    let mut mode = quote!(realtime());
    let mut nondeterministic = false;
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("ports")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("offline") {
                mode = quote!(offline());
            } else if meta.path.is_ident("nondeterministic") {
                nondeterministic = true;
            } else {
                return Err(meta.error("expected `offline` or `nondeterministic`"));
            }
            Ok(())
        })?;
    }

    let mut counts = [0usize; 4];
    let mut ports = Vec::new();
    for field in &fields.named {
        let name = field.ident.clone().expect("named field");
        let mut found = None;
        for attr in &field.attrs {
            let Some(kind) = Kind::of(attr) else { continue };
            if found.is_some() {
                return Err(syn::Error::new_spanned(
                    attr,
                    "a port has exactly one port attribute",
                ));
            }
            let args = attr.parse_args_with(Punctuated::<Expr, Token![,]>::parse_terminated)?;
            let want = if kind == Kind::Param { 3 } else { 2 };
            if args.len() != want {
                let usage = if kind == Kind::Param {
                    "(key, name, ParamInfo)"
                } else {
                    "(key, name)"
                };
                return Err(syn::Error::new_spanned(attr, format!("expected {usage}")));
            }
            let mut args = args.into_iter();
            let (key, display) = (lit_str(args.next())?, lit_str(args.next())?);
            found = Some((kind, key, display, args.next()));
        }
        let Some((kind, key, display, info)) = found else {
            return Err(syn::Error::new_spanned(
                field,
                "every field needs a port attribute",
            ));
        };
        let index = counts[kind.space()];
        counts[kind.space()] += 1;
        ports.push(Port {
            name,
            kind,
            key,
            display,
            info,
            index,
        });
    }

    let consts = ports.iter().map(|p| {
        let name = format_ident!("{}", p.name.to_string().to_uppercase());
        let index = p.index;
        quote!(pub const #name: usize = #index;)
    });
    let calls = ports.iter().map(|p| {
        let (key, display) = (&p.key, &p.display);
        match p.kind {
            Kind::Input => quote!(.input(#key, #display)),
            Kind::Param => {
                let info = p.info.as_ref();
                quote!(.param(#key, #display, #info))
            }
            Kind::Output => quote!(.output(#key, #display)),
            Kind::EventInput => quote!(.event_input(#key, #display)),
            Kind::EventOutput => quote!(.event_output(#key, #display)),
        }
    });
    let fields = ports.iter().map(|p| &p.name);
    let nondeterministic = nondeterministic.then(|| quote!(.nondeterministic()));
    let [inputs, outputs, event_inputs, event_outputs] = counts;
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    Ok(quote! {
        // The fields only name ports; reading them here keeps `dead_code`
        // from asking for each to be used.
        const _: () = {
            #[allow(dead_code)]
            fn read_fields #impl_generics (ports: &#name #ty_generics) #where_clause {
                let _ = (#(&ports.#fields,)*);
            }
        };

        #[allow(dead_code)]
        impl #impl_generics #name #ty_generics #where_clause {
            #(#consts)*
            pub const INPUTS: usize = #inputs;
            pub const OUTPUTS: usize = #outputs;
            pub const EVENT_INPUTS: usize = #event_inputs;
            pub const EVENT_OUTPUTS: usize = #event_outputs;

            /// The node's ports, in the order the constants index them.
            pub fn layout() -> ::noodle_engine::Layout {
                ::noodle_engine::Layout::#mode
                    #(#calls)*
                    #nondeterministic
            }
        }
    })
}

fn lit_str(expr: Option<Expr>) -> syn::Result<LitStr> {
    match expr {
        Some(Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        })) => Ok(s),
        Some(other) => Err(syn::Error::new_spanned(
            other,
            "the key and name must be string literals",
        )),
        None => unreachable!("argument count was checked"),
    }
}
