use proc_macro2::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Expr, LitStr, Path, Token};

/// Input: `crate_path, Level, emit_stack_dump, "format string", arg1, arg2, ...`
///
/// `crate_path` is the path to the `rcard_log` crate root (typically `$crate`
/// from the calling macro). The proc macro uses this to emit fully-qualified
/// paths that work even when `rcard_log` is accessed through a re-export chain
/// (e.g. `ipc::__rcard_log`).
///
/// `emit_stack_dump` is a boolean literal (`true` or `false`).  When `true`,
/// a stack dump is captured and appended to the log entry before
/// `TAG_END_OF_STREAM`.
pub struct SpeciesInput {
    pub krate: Path,
    pub level: Expr,
    pub emit_stack_dump: bool,
    pub format_str: LitStr,
    pub args: Vec<Expr>,
}

impl Parse for SpeciesInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let krate: Path = input.parse()?;
        let _: Token![,] = input.parse()?;
        let level: Expr = input.parse()?;
        let _: Token![,] = input.parse()?;
        let emit_stack_dump: syn::LitBool = input.parse()?;
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
            emit_stack_dump: emit_stack_dump.value(),
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

    let span = input.format_str.span();
    let file = span.file();
    let line = span.start().line;
    let column = span.start().column;

    // Generate a unique hash to use as the species ID.
    // Deterministic: same source location always produces the same hash.
    let hash = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        format_string.hash(&mut hasher);
        file.hash(&mut hasher);
        line.hash(&mut hasher);
        column.hash(&mut hasher);
        hasher.finish()
    };

    let hash_hex = format!("0x{:016x}", hash);
    let section_tokens = crate::section::emit(&serde_json::json!({
        "id": hash_hex,
        "entry": {
            "kind": "species",
            "format": format_string,
            "arg_count": arg_count,
            "file": file,
            "line": line,
            "column": column,
        },
    }));

    let hash_lit = hash;

    let stack_dump_code = if input.emit_stack_dump {
        quote! {
            {
                let __dump = #krate::stack_dump::capture();
                #krate::formatter::Format::format(&__dump, &mut __f);
            }
        }
    } else {
        quote! {}
    };

    quote! {
        {
            #section_tokens

            let __species_id: u64 = #hash_lit;
            let mut __writer = #krate::LogWriter::new(#level, __species_id);
            let mut __f = #krate::formatter::Formatter::new(&mut __writer);
            #(
                #krate::formatter::Format::format(&(#args), &mut __f);
            )*
            #stack_dump_code
            __f.write_end_of_stream();
            drop(__writer);
        }
    }
}
