use proc_macro2::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Expr, LitStr, Path, Token};

/// Input: `crate_path, Level, "format string", arg1, arg2, ...`
///
/// `crate_path` is the path to the `rcard_log` crate root (typically `$crate`
/// from the calling macro). The proc macro uses this to emit fully-qualified
/// paths that work even when `rcard_log` is accessed through a re-export chain
/// (e.g. `ipc::__rcard_log`).
pub struct SpeciesInput {
    pub krate: Path,
    pub level: Expr,
    pub format_str: LitStr,
    pub args: Vec<Expr>,
}

impl Parse for SpeciesInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let krate: Path = input.parse()?;
        let _: Token![,] = input.parse()?;
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
            krate,
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
    let krate = &input.krate;

    // Generate a unique hash to use as the species ID.
    let hash = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        format_string.hash(&mut hasher);
        // Mix in span debug repr for uniqueness when same format string used multiple times
        format!("{:?}", input.format_str.span()).hash(&mut hasher);
        hasher.finish()
    };

    // Write metadata to a sidecar JSON file (no-op if .work/ doesn't exist).
    // proc_macro2 span locations (line/column) are available with the
    // "span-locations" feature but source_file requires nightly, so we omit
    // the file path and let the user correlate via format string + line.
    let start = input.format_str.span().start();

    let species_json = serde_json::json!({
        "kind": "species",
        "format": format_string,
        "arg_count": arg_count,
        "line": start.line,
        "column": start.column,
    });
    let hash_hex = format!("0x{:016x}", hash);
    crate::sidecar::emit(
        &format!("species.{:016x}.json", hash),
        &serde_json::json!({
            "id": hash_hex,
            "entry": species_json,
        }),
    );

    let hash_lit = hash;

    quote! {
        {
            let __species_id: u64 = #hash_lit;
            let mut __writer = #krate::LogWriter::new(#level, __species_id);
            let mut __f = #krate::formatter::Formatter::new(&mut __writer);
            #(
                #krate::formatter::Format::format(&(#args), &mut __f);
            )*
            __f.write_end_of_stream();
            drop(__writer);
        }
    }
}
