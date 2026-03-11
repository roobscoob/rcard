use proc_macro2::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Expr, LitStr, Token};

/// Input: `Level, "format string", arg1, arg2, ...`
pub struct SpeciesInput {
    pub level: Expr,
    pub format_str: LitStr,
    pub args: Vec<Expr>,
}

impl Parse for SpeciesInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let level: Expr = input.parse()?;
        let _: Token![,] = input.parse()?;
        let format_str: LitStr = input.parse()?;
        let mut args = Vec::new();
        while input.peek(Token![,]) {
            let _: Token![,] = input.parse()?;
            if input.is_empty() {
                break;
            }
            args.push(input.parse()?);
        }
        Ok(SpeciesInput {
            level,
            format_str,
            args,
        })
    }
}

pub fn expand_species(input: SpeciesInput) -> TokenStream {
    let format_string = input.format_str.value();
    let arg_count = input.args.len();
    let args = &input.args;
    let level = &input.level;

    // Build species JSON metadata
    let species_json = serde_json::json!({
        "kind": "species",
        "format": format_string,
        "arg_count": arg_count,
    });
    let json_str = format!("{}\0", species_json);
    let json_bytes: Vec<_> = json_str.as_bytes().iter().map(|b| quote! { #b }).collect();
    let json_len = json_str.len();

    // Generate a unique static name using a content hash
    let hash = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        format_string.hash(&mut hasher);
        // Mix in span debug repr for uniqueness when same format string used multiple times
        format!("{:?}", input.format_str.span()).hash(&mut hasher);
        hasher.finish()
    };
    let static_name = quote::format_ident!("__RCARD_LOG_SPECIES_{:016x}", hash);

    quote! {
        {
            #[allow(non_upper_case_globals)]
            #[unsafe(link_section = ".rcard_log")]
            #[used]
            static #static_name: [u8; #json_len] = [#(#json_bytes),*];

            let __species_id: u64 = &#static_name as *const _ as u64;
            let mut __writer = rcard_log::LogWriter::new(#level, __species_id);
            let mut __f = rcard_log::formatter::Formatter::new(&mut __writer);
            #(
                rcard_log::formatter::Format::format(&(#args), &mut __f);
            )*
            __f.write_end_of_stream();
            drop(__f);
            drop(__writer);
        }
    }
}
