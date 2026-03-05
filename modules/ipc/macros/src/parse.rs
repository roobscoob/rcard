use syn::{
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
    Expr, FnArg, Ident, ItemTrait, Lit, Meta, Pat, ReturnType, Token, TraitItem,
    TraitItemFn, Type,
};

// ---------------------------------------------------------------------------
// Attribute parsing: #[ipc::resource(arena_size = N, kind = 0xKK)]
// ---------------------------------------------------------------------------

pub struct ResourceAttr {
    pub arena_size: usize,
    pub kind: u8,
}

impl Parse for ResourceAttr {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut arena_size = None;
        let mut kind = None;

        let pairs = Punctuated::<Meta, Token![,]>::parse_terminated(input)?;
        for meta in pairs {
            if let Meta::NameValue(nv) = meta {
                let ident = nv.path.get_ident().map(|i| i.to_string());
                match ident.as_deref() {
                    Some("arena_size") => {
                        if let Expr::Lit(lit) = &nv.value {
                            if let Lit::Int(i) = &lit.lit {
                                arena_size = Some(i.base10_parse::<usize>()?);
                            }
                        }
                    }
                    Some("kind") => {
                        if let Expr::Lit(lit) = &nv.value {
                            if let Lit::Int(i) = &lit.lit {
                                kind = Some(i.base10_parse::<u8>()?);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        Ok(ResourceAttr {
            arena_size: arena_size.ok_or_else(|| input.error("missing `arena_size`"))?,
            kind: kind.ok_or_else(|| input.error("missing `kind`"))?,
        })
    }
}

// ---------------------------------------------------------------------------
// Method classification
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MethodKind {
    Constructor,
    Message,
    Destructor,
}

pub struct ParsedParam {
    pub name: Ident,
    pub ty: Type,
    pub is_lease: bool,
    pub lease_mutable: bool,
}

pub struct ParsedMethod {
    pub kind: MethodKind,
    pub name: Ident,
    pub params: Vec<ParsedParam>,
    pub return_type: Option<Type>,
    pub method_id: u8,
}

fn classify_method(method: &TraitItemFn) -> Option<MethodKind> {
    for attr in &method.attrs {
        if attr.path().is_ident("constructor") {
            return Some(MethodKind::Constructor);
        }
        if attr.path().is_ident("message") {
            return Some(MethodKind::Message);
        }
        if attr.path().is_ident("destructor") {
            return Some(MethodKind::Destructor);
        }
    }
    None
}

fn is_lease_param(arg: &FnArg) -> bool {
    if let FnArg::Typed(pat_type) = arg {
        pat_type.attrs.iter().any(|a| a.path().is_ident("lease"))
    } else {
        false
    }
}

/// Check if a type is `&[T]` or `&mut [T]`.
/// Returns (inner_type, is_mutable).
pub fn parse_slice_ref(ty: &Type) -> Option<(Type, bool)> {
    if let Type::Reference(r) = ty {
        let mutable = r.mutability.is_some();
        if let Type::Slice(s) = r.elem.as_ref() {
            return Some((*s.elem.clone(), mutable));
        }
    }
    None
}

fn extract_param(arg: &FnArg) -> Option<ParsedParam> {
    if let FnArg::Typed(pat_type) = arg {
        let name = if let Pat::Ident(pi) = pat_type.pat.as_ref() {
            pi.ident.clone()
        } else {
            return None;
        };

        let is_lease = is_lease_param(arg);
        let mut lease_mutable = false;

        if is_lease {
            if let Some((_, m)) = parse_slice_ref(&pat_type.ty) {
                lease_mutable = m;
            }
        }

        Some(ParsedParam {
            name,
            ty: (*pat_type.ty).clone(),
            is_lease,
            lease_mutable,
        })
    } else {
        None
    }
}

fn extract_return_type(ret: &ReturnType) -> Option<Type> {
    match ret {
        ReturnType::Default => None,
        ReturnType::Type(_, ty) => {
            if let Type::Path(p) = ty.as_ref() {
                if p.path.is_ident("Self") {
                    return None;
                }
            }
            Some((*ty.clone()).clone())
        }
    }
}

pub fn parse_methods(trait_def: &ItemTrait) -> Vec<ParsedMethod> {
    let mut methods = Vec::new();
    let mut next_id: u8 = 0;

    for item in &trait_def.items {
        if let TraitItem::Fn(method) = item {
            let Some(kind) = classify_method(method) else {
                continue;
            };

            let mut params = Vec::new();
            for arg in &method.sig.inputs {
                if matches!(arg, FnArg::Receiver(_)) {
                    continue;
                }
                if let Some(p) = extract_param(arg) {
                    params.push(p);
                }
            }

            let return_type = extract_return_type(&method.sig.output);

            methods.push(ParsedMethod {
                kind,
                name: method.sig.ident.clone(),
                params,
                return_type,
                method_id: next_id,
            });

            next_id += 1;
        }
    }

    methods
}
