use syn::{
    ext::IdentExt,
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
    Expr, FnArg, GenericArgument, Ident, ItemTrait, Lit, Meta, Pat, PathArguments,
    ReturnType, Token, TraitItem, TraitItemFn, Type, TypeParamBound,
};

// ---------------------------------------------------------------------------
// Attribute parsing: #[ipc::resource(arena_size = N, kind = 0xKK, ...)]
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CloneMode {
    Refcount,
}

pub struct ResourceAttr {
    pub arena_size: Option<usize>,
    pub kind: u8,
    /// Interface trait this resource implements. The macro will reference the
    /// interface's Op enum to assign matching method IDs at compile time.
    pub implements: Option<syn::Path>,
    pub clone_mode: Option<CloneMode>,
    pub parent: Option<String>,
}

impl Parse for ResourceAttr {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut arena_size = None;
        let mut kind = None;
        let mut implements: Option<syn::Path> = None;
        let mut clone_mode = None;
        let mut parent = None;

        let pairs = Punctuated::<Meta, Token![,]>::parse_terminated(input)?;
        for meta in pairs {
            if let Meta::NameValue(nv) = &meta {
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
                    Some("clone") => {
                        if let Expr::Path(p) = &nv.value {
                            if p.path.is_ident("refcount") {
                                clone_mode = Some(CloneMode::Refcount);
                            }
                        }
                    }
                    Some("parent") => {
                        if let Expr::Path(p) = &nv.value {
                            if let Some(ident) = p.path.get_ident() {
                                parent = Some(ident.to_string());
                            }
                        }
                    }
                    other => {
                        return Err(syn::Error::new_spanned(
                            &nv.path,
                            format!(
                                "unknown ipc::resource attribute `{}`; \
                                 expected one of: arena_size, kind, clone, parent",
                                other.unwrap_or("?"),
                            ),
                        ));
                    }
                }
            } else if let Meta::List(list) = &meta {
                if list.path.is_ident("implements") {
                    // Parse implements(path::to::Trait)
                    let path: syn::Path = syn::parse2(list.tokens.clone()).map_err(|_| {
                        syn::Error::new_spanned(
                            &list.tokens,
                            "expected `implements(TraitPath)`",
                        )
                    })?;
                    implements = Some(path);
                } else {
                    return Err(syn::Error::new_spanned(
                        &list.path,
                        format!(
                            "unknown ipc::resource attribute `{}`; \
                             expected one of: arena_size, kind, clone, parent, implements",
                            list.path.get_ident().map(|i| i.to_string()).unwrap_or_else(|| "?".into()),
                        ),
                    ));
                }
            } else {
                return Err(syn::Error::new_spanned(
                    &meta,
                    "unexpected attribute form; use `key = value` or `implements([...])`",
                ));
            }
        }

        Ok(ResourceAttr {
            arena_size,
            kind: kind.ok_or_else(|| input.error("missing `kind`"))?,
            implements,
            clone_mode,
            parent,
        })
    }
}

// ---------------------------------------------------------------------------
// Attribute parsing: #[ipc::interface(kind = 0xKK)]
// ---------------------------------------------------------------------------

pub struct InterfaceAttr {
    pub kind: u8,
}

impl Parse for InterfaceAttr {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut kind = None;

        let pairs = Punctuated::<Meta, Token![,]>::parse_terminated(input)?;
        for meta in pairs {
            if let Meta::NameValue(nv) = &meta {
                let ident = nv.path.get_ident().map(|i| i.to_string());
                match ident.as_deref() {
                    Some("kind") => {
                        if let Expr::Lit(lit) = &nv.value {
                            if let Lit::Int(i) = &lit.lit {
                                kind = Some(i.base10_parse::<u8>()?);
                            }
                        }
                    }
                    other => {
                        return Err(syn::Error::new_spanned(
                            &nv.path,
                            format!(
                                "unknown ipc::interface attribute `{}`; expected `kind`",
                                other.unwrap_or("?"),
                            ),
                        ));
                    }
                }
            } else {
                return Err(syn::Error::new_spanned(
                    &meta,
                    "unexpected attribute form; use `kind = 0xNN`",
                ));
            }
        }

        Ok(InterfaceAttr {
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
    StaticMessage,
    Destructor,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HandleMode {
    Move,
    Clone,
}

/// Parsed constructor return type. Constructors may only return one of these
/// three forms; any other return type is a compile error.
pub enum ConstructorReturn {
    /// `-> Self` (or no return type) — infallible.
    Bare,
    /// `-> Result<Self, E>` — may fail with domain error `E`.
    Result(Type),
    /// `-> Option<Self>` — may return None.
    OptionSelf,
}

pub struct ParsedParam {
    pub name: Ident,
    pub ty: Type,
    pub is_lease: bool,
    pub lease_mutable: bool,
    pub handle_mode: Option<HandleMode>,
    /// If the type is `impl Trait`, this is the trait name.
    pub impl_trait_name: Option<Ident>,
}

pub struct ParsedMethod {
    pub kind: MethodKind,
    pub name: Ident,
    pub params: Vec<ParsedParam>,
    pub return_type: Option<Type>,
    /// Only set for constructors.
    pub ctor_return: Option<ConstructorReturn>,
    pub method_id: u8,
    /// If this message constructs a different resource type.
    /// `(TraitName, GenericIdent)` from `#[message(constructs(FileSystem = FS))]`.
    pub constructs: Option<(Ident, Ident)>,
    /// If the return type is `impl Trait`.
    pub return_impl_trait: Option<Ident>,
}

fn has_receiver(method: &TraitItemFn) -> bool {
    method.sig.inputs.iter().any(|arg| matches!(arg, FnArg::Receiver(_)))
}

fn classify_method(method: &TraitItemFn) -> Option<(MethodKind, Option<(Ident, Ident)>)> {
    for attr in &method.attrs {
        if attr.path().is_ident("constructor") {
            return Some((MethodKind::Constructor, None));
        }
        if attr.path().is_ident("message") {
            let constructs = parse_constructs_attr(attr);
            if has_receiver(method) {
                return Some((MethodKind::Message, constructs));
            } else {
                return Some((MethodKind::StaticMessage, constructs));
            }
        }
        if attr.path().is_ident("destructor") {
            return Some((MethodKind::Destructor, None));
        }
    }
    None
}

/// Parse `#[message(constructs(TraitName = GenericIdent))]`.
fn parse_constructs_attr(attr: &syn::Attribute) -> Option<(Ident, Ident)> {
    let list = attr.meta.require_list().ok()?;
    syn::parse2::<ConstructsArg>(list.tokens.clone())
        .ok()
        .map(|a| (a.trait_name, a.generic_ident))
}

struct ConstructsArg {
    trait_name: Ident,
    generic_ident: Ident,
}

impl Parse for ConstructsArg {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let key: Ident = input.parse()?;
        if key != "constructs" {
            return Err(syn::Error::new(key.span(), "expected `constructs`"));
        }
        // Parse the inner parens: `(TraitName = GenericIdent)`
        let content;
        syn::parenthesized!(content in input);
        let trait_name: Ident = content.parse()?;
        content.parse::<Token![=]>()?;
        let generic_ident: Ident = content.parse()?;
        Ok(ConstructsArg {
            trait_name,
            generic_ident,
        })
    }
}

fn is_lease_param(arg: &FnArg) -> bool {
    if let FnArg::Typed(pat_type) = arg {
        pat_type.attrs.iter().any(|a| a.path().is_ident("lease"))
    } else {
        false
    }
}

fn parse_handle_mode(arg: &FnArg) -> Option<HandleMode> {
    if let FnArg::Typed(pat_type) = arg {
        for attr in &pat_type.attrs {
            if attr.path().is_ident("handle") {
                if let Ok(mode) = attr.parse_args_with(|input: ParseStream| {
                    let ident = input.call(Ident::parse_any)?;
                    match ident.to_string().as_str() {
                        "move" => Ok(HandleMode::Move),
                        "clone" => Ok(HandleMode::Clone),
                        _ => Err(syn::Error::new(
                            ident.span(),
                            "expected `move` or `clone`",
                        )),
                    }
                }) {
                    return Some(mode);
                }
            }
        }
    }
    None
}

/// Extract the trait name from `impl Trait` types.
/// For `impl Foo` returns `Foo`; for `impl some::path::Foo` returns `Foo`.
fn extract_impl_trait(ty: &Type) -> Option<Ident> {
    if let Type::ImplTrait(impl_trait) = ty {
        for bound in &impl_trait.bounds {
            if let TypeParamBound::Trait(trait_bound) = bound {
                return trait_bound.path.segments.last().map(|s| s.ident.clone());
            }
        }
    }
    None
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

        let handle_mode = parse_handle_mode(arg);
        let impl_trait_name = extract_impl_trait(&pat_type.ty);

        Some(ParsedParam {
            name,
            ty: (*pat_type.ty).clone(),
            is_lease,
            lease_mutable,
            handle_mode,
            impl_trait_name,
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
            Some((**ty).clone())
        }
    }
}

fn extract_return_impl_trait(ret: &ReturnType) -> Option<Ident> {
    match ret {
        ReturnType::Default => None,
        ReturnType::Type(_, ty) => {
            // Check bare `impl Trait`
            if let Some(name) = extract_impl_trait(ty) {
                return Some(name);
            }
            // Check `Result<impl Trait, E>` and `Option<impl Trait>`
            if let Type::Path(p) = ty.as_ref() {
                if let Some(seg) = p.path.segments.last() {
                    if let PathArguments::AngleBracketed(args) = &seg.arguments {
                        if let Some(GenericArgument::Type(inner)) = args.args.first() {
                            return extract_impl_trait(inner);
                        }
                    }
                }
            }
            None
        }
    }
}

/// Parse a constructor's return type into a validated `ConstructorReturn`.
/// Only `Self`, `Result<Self, E>`, and `Option<Self>` are accepted.
fn parse_ctor_return(ret: &ReturnType) -> syn::Result<ConstructorReturn> {
    match ret {
        ReturnType::Default => Ok(ConstructorReturn::Bare),
        ReturnType::Type(_, ty) => classify_ctor_type(ty),
    }
}

fn classify_ctor_type(ty: &Type) -> syn::Result<ConstructorReturn> {
    if let Type::Path(p) = ty {
        // Bare `Self`
        if p.path.is_ident("Self") {
            return Ok(ConstructorReturn::Bare);
        }

        if let Some(seg) = p.path.segments.last() {
            // `Result<Self, E>`
            if seg.ident == "Result" {
                if let PathArguments::AngleBracketed(args) = &seg.arguments {
                    let args: Vec<_> = args.args.iter().collect();
                    if args.len() == 2 {
                        if let GenericArgument::Type(Type::Path(first)) = &args[0] {
                            if first.path.is_ident("Self") {
                                if let GenericArgument::Type(err_ty) = &args[1] {
                                    return Ok(ConstructorReturn::Result(err_ty.clone()));
                                }
                            }
                        }
                    }
                }
            }

            // `Option<Self>`
            if seg.ident == "Option" {
                if let PathArguments::AngleBracketed(args) = &seg.arguments {
                    let args: Vec<_> = args.args.iter().collect();
                    if args.len() == 1 {
                        if let GenericArgument::Type(Type::Path(inner)) = &args[0] {
                            if inner.path.is_ident("Self") {
                                return Ok(ConstructorReturn::OptionSelf);
                            }
                        }
                    }
                }
            }
        }
    }

    Err(syn::Error::new_spanned(
        ty,
        "constructor must return `Self`, `Result<Self, E>`, or `Option<Self>`",
    ))
}

pub fn parse_methods(trait_def: &ItemTrait) -> syn::Result<Vec<ParsedMethod>> {
    let mut methods = Vec::new();
    let mut next_id: u8 = 0;

    for item in &trait_def.items {
        if let TraitItem::Fn(method) = item {
            let Some((kind, constructs)) = classify_method(method) else {
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
            let return_impl_trait = extract_return_impl_trait(&method.sig.output);

            let ctor_return = if kind == MethodKind::Constructor {
                Some(parse_ctor_return(&method.sig.output)?)
            } else {
                None
            };

            // 0xFD, 0xFE, 0xFF are reserved.
            if next_id >= 0xFD {
                return Err(syn::Error::new(
                    method.sig.ident.span(),
                    "too many methods: method IDs 0xFD-0xFF are reserved",
                ));
            }

            methods.push(ParsedMethod {
                kind,
                name: method.sig.ident.clone(),
                params,
                return_type,
                ctor_return,
                method_id: next_id,
                constructs,
                return_impl_trait,
            });

            next_id += 1;
        }
    }

    Ok(methods)
}

/// Given an interface trait path like `storage_api::Storage`, construct the
/// corresponding Op enum path: `storage_api::StorageOp`.
pub fn interface_op_path(iface_path: &syn::Path) -> syn::Path {
    use quote::format_ident;
    let mut op_path = iface_path.clone();
    if let Some(last) = op_path.segments.last_mut() {
        last.ident = format_ident!("{}Op", last.ident);
    }
    op_path
}
