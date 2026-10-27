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
///   `#[ports(memoryless)]` and `#[ports(bounded = MS)]` say how long the
///   node remembers its input (see `noodle_engine::Memory`); without one it
///   is unbounded. `#[ports(cost = N)]` says what a second of the node costs
///   to render against a gain stage's 1 (the playback cache's cost threshold).
/// - An index constant per field, named after it in upper case (`Ports::GAIN`).
///   Audio and parameter inputs share one index space, in field order, as do
///   `output`, `event_input`, `event_output`, `spectral_input`,
///   `spectral_output`, `sample_input` and `sample_output` ports among their own kind.
/// - `Ports::INPUTS`, `Ports::OUTPUTS`, `Ports::EVENT_INPUTS` and
///   `Ports::EVENT_OUTPUTS`, `Ports::SPECTRAL_INPUTS` and `Ports::SPECTRAL_OUTPUTS`, the port counts.
///
/// Field attributes: `#[input(key, name)]`, `#[param(key, name, ParamInfo)]`,
/// `#[output(key, name)]`, `#[control_output(key, name)]` (an output tagged as
/// a control signal, shown as such in the editor; it shares `output`'s indices),
/// `#[event_input(key, name)]` and
/// `#[event_output(key, name)]`, `#[spectral_input(key, name)]` and
/// `#[spectral_output(key, name)]`, `#[sample_input(key, name)]` and
/// `#[sample_output(key, name)]`. Nodes whose ports depend on config, such as
/// Mix, build their `Layout` by hand instead.
#[proc_macro_derive(
    Ports,
    attributes(
        input,
        param,
        output,
        control_output,
        event_input,
        event_output,
        spectral_input,
        spectral_output,
        sample_input,
        sample_output,
        ports
    )
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
    ControlOutput,
    EventInput,
    EventOutput,
    SpectralInput,
    SpectralOutput,
    SampleInput,
    SampleOutput,
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
        } else if path.is_ident("control_output") {
            Kind::ControlOutput
        } else if path.is_ident("event_input") {
            Kind::EventInput
        } else if path.is_ident("event_output") {
            Kind::EventOutput
        } else if path.is_ident("spectral_input") {
            Kind::SpectralInput
        } else if path.is_ident("spectral_output") {
            Kind::SpectralOutput
        } else if path.is_ident("sample_input") {
            Kind::SampleInput
        } else if path.is_ident("sample_output") {
            Kind::SampleOutput
        } else {
            return None;
        })
    }

    /// Which index space the kind's ports are counted in.
    fn space(self) -> usize {
        match self {
            Kind::Input | Kind::Param => 0,
            Kind::Output | Kind::ControlOutput => 1,
            Kind::EventInput => 2,
            Kind::EventOutput => 3,
            Kind::SpectralInput => 4,
            Kind::SpectralOutput => 5,
            Kind::SampleInput => 6,
            Kind::SampleOutput => 7,
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
    let mut memory = None;
    let mut cost = None;
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("ports")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("offline") {
                mode = quote!(offline());
            } else if meta.path.is_ident("nondeterministic") {
                nondeterministic = true;
            } else if meta.path.is_ident("memoryless") {
                memory = Some(quote!(.memoryless()));
            } else if meta.path.is_ident("unbounded") {
                memory = Some(quote!());
            } else if meta.path.is_ident("bounded") {
                let ms: syn::LitInt = meta.value()?.parse()?;
                memory = Some(quote!(.bounded(#ms)));
            } else if meta.path.is_ident("cost") {
                let units: syn::LitInt = meta.value()?.parse()?;
                cost = Some(quote!(.cost(#units)));
            } else {
                return Err(meta.error(
                    "expected `offline`, `nondeterministic`, `memoryless`, `unbounded`, `bounded = ms` or `cost = n`",
                ));
            }
            Ok(())
        })?;
    }

    let mut counts = [0usize; 8];
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
            Kind::ControlOutput => quote!(.control_output(#key, #display)),
            Kind::EventInput => quote!(.event_input(#key, #display)),
            Kind::EventOutput => quote!(.event_output(#key, #display)),
            Kind::SpectralInput => quote!(.spectral_input(#key, #display)),
            Kind::SpectralOutput => quote!(.spectral_output(#key, #display)),
            Kind::SampleInput => quote!(.sample_input(#key, #display)),
            Kind::SampleOutput => quote!(.sample_output(#key, #display)),
        }
    });
    let fields = ports.iter().map(|p| &p.name);
    let nondeterministic = nondeterministic.then(|| quote!(.nondeterministic()));
    let memory = memory.unwrap_or_default();
    let cost = cost.unwrap_or_default();
    let [
        inputs,
        outputs,
        event_inputs,
        event_outputs,
        spectral_inputs,
        spectral_outputs,
        sample_inputs,
        sample_outputs,
    ] = counts;
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
            pub const SPECTRAL_INPUTS: usize = #spectral_inputs;
            pub const SPECTRAL_OUTPUTS: usize = #spectral_outputs;
            pub const SAMPLE_INPUTS: usize = #sample_inputs;
            pub const SAMPLE_OUTPUTS: usize = #sample_outputs;

            /// The node's ports, in the order the constants index them.
            pub fn layout() -> ::noodle_engine::Layout {
                ::noodle_engine::Layout::#mode
                    #(#calls)*
                    #nondeterministic
                    #memory
                    #cost
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
