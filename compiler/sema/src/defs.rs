//! The definition table, module scopes, collection of items, `use`
//! resolution and visibility (spec/semantics.md, section 2).

use crate::diag::Diags;
use crate::source::{FileId, SourceFile};
use pegasus_parser::ast::{self, Item, ItemKind, Span, UseTree};
use std::collections::{HashMap, HashSet};

pub type DefId = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefKind {
    Mod,
    Struct,
    Enum,
    Variant,
    Trait,
    TypeAlias,
    Fn,
    ExternFn,
    Const,
    Impl,
    AssocType,
    /// A generic type or const parameter (including a trait's `Self`).
    GenericParam,
    Prim,
}

impl DefKind {
    pub fn describe(self) -> &'static str {
        match self {
            DefKind::Mod => "module",
            DefKind::Struct => "struct",
            DefKind::Enum => "enum",
            DefKind::Variant => "enum variant",
            DefKind::Trait => "trait",
            DefKind::TypeAlias => "type alias",
            DefKind::Fn => "function",
            DefKind::ExternFn => "extern function",
            DefKind::Const => "constant",
            DefKind::Impl => "impl",
            DefKind::AssocType => "associated type",
            DefKind::GenericParam => "generic parameter",
            DefKind::Prim => "primitive type",
        }
    }
    pub fn is_type_ns(self) -> bool {
        matches!(
            self,
            DefKind::Mod
                | DefKind::Struct
                | DefKind::Enum
                | DefKind::Trait
                | DefKind::TypeAlias
                | DefKind::Prim
                | DefKind::AssocType
                | DefKind::GenericParam
        )
    }
}

pub struct Def<'a> {
    pub kind: DefKind,
    pub name: String,
    /// Enclosing definition: module, enum (variants), impl or trait (members),
    /// or the item owning a generic parameter.
    pub parent: Option<DefId>,
    /// The module whose privacy rules apply.
    pub module: DefId,
    pub file: FileId,
    pub span: Span,
    pub is_pub: bool,
    pub item: Option<&'a Item>,
    pub variant: Option<(&'a ast::Variant, usize)>,
    pub gparam: Option<&'a ast::GenericParam>,
}

#[derive(Clone, Debug)]
pub struct Binding {
    pub def: DefId,
    /// Visible outside the module that holds the binding.
    pub is_pub: bool,
    pub file: FileId,
    pub span: Span,
}

/// A lookup result: a binding, nothing, or several glob-imported candidates.
pub type Lookup = Result<Option<Binding>, Vec<Binding>>;

#[derive(Default)]
pub struct ModData {
    pub parent: Option<DefId>,
    pub types: HashMap<String, Binding>,
    pub values: HashMap<String, Binding>,
    /// Names brought in by glob imports; several distinct definitions under
    /// one name are ambiguous only when the name is used (R005).
    pub glob_types: HashMap<String, Vec<Binding>>,
    pub glob_values: HashMap<String, Vec<Binding>>,
    pub file: FileId,
    pub package_root: DefId,
}

#[derive(Clone, Debug)]
pub enum ImportKind {
    Single(String),
    Glob,
}

#[derive(Clone, Debug)]
pub struct Import {
    pub module: DefId,
    pub path: Vec<String>,
    pub kind: ImportKind,
    pub is_pub: bool,
    pub file: FileId,
    pub span: Span,
}

#[derive(Clone, Debug)]
struct Glob {
    module: DefId,
    target: DefId,
    is_pub: bool,
    file: FileId,
    span: Span,
}

/// What a name resolves to, per namespace.
#[derive(Clone, Debug, Default)]
pub struct PerNs {
    pub ty: Option<Binding>,
    pub value: Option<Binding>,
}

pub const PRIMS: [&str; 19] = [
    "bool", "char", "str", "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64",
    "u128", "usize", "f16", "bf16", "f32", "f64",
];

pub struct DefTable<'a> {
    pub files: &'a [SourceFile],
    pub defs: Vec<Def<'a>>,
    pub mods: HashMap<DefId, ModData>,
    pub variants: HashMap<DefId, Vec<DefId>>,
    /// Members of an impl or trait by name.
    pub members: HashMap<DefId, Vec<DefId>>,
    pub impls: Vec<DefId>,
    pub traits: Vec<DefId>,
    pub prims: HashMap<String, DefId>,
    pub core_root: DefId,
    pub root: DefId,
    imports: Vec<Import>,
    globs: Vec<Glob>,
}

impl<'a> DefTable<'a> {
    pub fn def(&self, d: DefId) -> &Def<'a> {
        &self.defs[d as usize]
    }

    pub fn add(&mut self, def: Def<'a>) -> DefId {
        self.defs.push(def);
        (self.defs.len() - 1) as DefId
    }

    /// Collect both packages: the built-in prelude (file 0) and the user
    /// package (root file 1). `mod_files` maps a body-less `mod` item
    /// (file, item start) to the file it loaded.
    pub fn collect(
        files: &'a [SourceFile],
        mod_files: &HashMap<(FileId, u32), FileId>,
        diags: &mut Diags,
    ) -> DefTable<'a> {
        let mut t = DefTable {
            files,
            defs: Vec::new(),
            mods: HashMap::new(),
            variants: HashMap::new(),
            members: HashMap::new(),
            impls: Vec::new(),
            traits: Vec::new(),
            prims: HashMap::new(),
            core_root: 0,
            root: 0,
            imports: Vec::new(),
            globs: Vec::new(),
        };
        for p in PRIMS {
            let d = t.add(Def {
                kind: DefKind::Prim,
                name: p.to_string(),
                parent: None,
                module: 0,
                file: 0,
                span: (0, 0),
                is_pub: true,
                item: None,
                variant: None,
                gparam: None,
            });
            t.prims.insert(p.to_string(), d);
        }
        for (file, name) in [(0u32, "core"), (1u32, "<root>")] {
            let root = t.add(Def {
                kind: DefKind::Mod,
                name: name.to_string(),
                parent: None,
                module: 0,
                file,
                span: (0, 0),
                is_pub: true,
                item: None,
                variant: None,
                gparam: None,
            });
            t.defs[root as usize].module = root;
            t.mods.insert(
                root,
                ModData {
                    file,
                    package_root: root,
                    ..Default::default()
                },
            );
            if file == 0 {
                t.core_root = root;
            } else {
                t.root = root;
            }
            let items: &'a [Item] = &files[file as usize].module.items;
            t.collect_items(root, file, items, mod_files, diags);
        }
        t
    }

    fn collect_items(
        &mut self,
        m: DefId,
        file: FileId,
        items: &'a [Item],
        mod_files: &HashMap<(FileId, u32), FileId>,
        diags: &mut Diags,
    ) {
        let files = self.files;
        for item in items {
            let mk = |kind, name: &str, parent| Def {
                kind,
                name: name.to_string(),
                parent: Some(parent),
                module: m,
                file,
                span: item.span,
                is_pub: item.is_pub,
                item: Some(item),
                variant: None,
                gparam: None,
            };
            match &item.kind {
                ItemKind::Use(tree) => {
                    let mut flat = Vec::new();
                    flatten_use(tree, &[], &mut flat);
                    for (path, kind) in flat {
                        self.imports.push(Import {
                            module: m,
                            path,
                            kind,
                            is_pub: item.is_pub,
                            file,
                            span: item.span,
                        });
                    }
                }
                ItemKind::Mod { name, items: body } => {
                    let d = self.add(mk(DefKind::Mod, name, m));
                    self.defs[d as usize].module = d;
                    let root = self.mods[&m].package_root;
                    let (child_file, child_items): (FileId, Option<&'a [Item]>) = match body {
                        Some(v) => (file, Some(v.as_slice())),
                        None => match mod_files.get(&(file, item.span.0)) {
                            Some(&f) => (f, Some(files[f as usize].module.items.as_slice())),
                            None => (file, None),
                        },
                    };
                    self.mods.insert(
                        d,
                        ModData {
                            parent: Some(m),
                            file: child_file,
                            package_root: root,
                            ..Default::default()
                        },
                    );
                    self.bind_item(m, name, d, true, diags);
                    if let Some(ci) = child_items {
                        self.collect_items(d, child_file, ci, mod_files, diags);
                    }
                }
                ItemKind::Fn(f) => {
                    let d = self.add(mk(DefKind::Fn, &f.sig.name, m));
                    self.bind_item(m, &f.sig.name, d, false, diags);
                }
                ItemKind::ExternFn { sig, .. } => {
                    let d = self.add(mk(DefKind::ExternFn, &sig.name, m));
                    self.bind_item(m, &sig.name, d, false, diags);
                }
                ItemKind::Const { name, .. } => {
                    let d = self.add(mk(DefKind::Const, name, m));
                    self.bind_item(m, name, d, false, diags);
                }
                ItemKind::Struct { name, .. } => {
                    let d = self.add(mk(DefKind::Struct, name, m));
                    self.bind_item(m, name, d, true, diags);
                }
                ItemKind::TypeAlias { name, .. } => {
                    let d = self.add(mk(DefKind::TypeAlias, name, m));
                    self.bind_item(m, name, d, true, diags);
                }
                ItemKind::Enum { name, variants, .. } => {
                    let d = self.add(mk(DefKind::Enum, name, m));
                    self.bind_item(m, name, d, true, diags);
                    let mut vs = Vec::new();
                    let mut seen: HashSet<&str> = HashSet::new();
                    for (i, v) in variants.iter().enumerate() {
                        if !seen.insert(&v.name) {
                            diags.error(
                                "R001",
                                file,
                                v.span,
                                format!("variant `{}` is defined more than once", v.name),
                            );
                        }
                        let vd = self.add(Def {
                            kind: DefKind::Variant,
                            name: v.name.clone(),
                            parent: Some(d),
                            module: m,
                            file,
                            span: v.span,
                            is_pub: true,
                            item: Some(item),
                            variant: Some((v, i)),
                            gparam: None,
                        });
                        vs.push(vd);
                    }
                    self.variants.insert(d, vs);
                }
                ItemKind::Trait {
                    name, items: body, ..
                } => {
                    let d = self.add(mk(DefKind::Trait, name, m));
                    self.bind_item(m, name, d, true, diags);
                    self.traits.push(d);
                    self.collect_members(d, m, file, body, diags, true);
                }
                ItemKind::Impl { items: body, .. } => {
                    let d = self.add(mk(DefKind::Impl, "<impl>", m));
                    self.impls.push(d);
                    self.collect_members(d, m, file, body, diags, false);
                }
                ItemKind::AssocType { .. } | ItemKind::Error => {}
            }
        }
    }

    fn collect_members(
        &mut self,
        owner: DefId,
        m: DefId,
        file: FileId,
        body: &'a [Item],
        diags: &mut Diags,
        in_trait: bool,
    ) {
        let mut out = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for it in body {
            let (kind, name) = match &it.kind {
                ItemKind::Fn(f) => (DefKind::Fn, f.sig.name.clone()),
                ItemKind::Const { name, .. } => (DefKind::Const, name.clone()),
                ItemKind::AssocType { name, .. } => (DefKind::AssocType, name.clone()),
                ItemKind::TypeAlias { name, .. } => (DefKind::AssocType, name.clone()),
                _ => continue,
            };
            if !seen.insert(name.clone()) {
                diags.error(
                    "R001",
                    file,
                    it.span,
                    format!("`{name}` is defined more than once in this block"),
                );
            }
            out.push(self.add(Def {
                kind,
                name,
                parent: Some(owner),
                module: m,
                file,
                span: it.span,
                // Trait members are as visible as the trait itself.
                is_pub: in_trait || it.is_pub,
                item: Some(it),
                variant: None,
                gparam: None,
            }));
        }
        self.members.insert(owner, out);
    }

    fn bind_item(&mut self, m: DefId, name: &str, d: DefId, type_ns: bool, diags: &mut Diags) {
        let def = &self.defs[d as usize];
        let b = Binding {
            def: d,
            is_pub: def.is_pub,
            file: def.file,
            span: def.span,
        };
        let md = self.mods.get_mut(&m).unwrap();
        let ns = if type_ns {
            &mut md.types
        } else {
            &mut md.values
        };
        if let Some(prev) = ns.get(name) {
            let (pf, ps) = (prev.file, prev.span);
            let _ = (pf, ps);
            diags.error(
                "R001",
                b.file,
                b.span,
                format!("`{name}` is defined more than once in this module"),
            );
            return;
        }
        ns.insert(name.to_string(), b);
    }

    // ------------------------------------------------------------ visibility

    pub fn is_within(&self, mut m: DefId, ancestor: DefId) -> bool {
        loop {
            if m == ancestor {
                return true;
            }
            match self.mods.get(&m).and_then(|d| d.parent) {
                Some(p) => m = p,
                None => return false,
            }
        }
    }

    /// A binding held by module `holder` may be used from module `from`.
    pub fn binding_visible(&self, b: &Binding, holder: DefId, from: DefId) -> bool {
        b.is_pub || self.is_within(from, holder)
    }

    /// A definition (field-level privacy aside) may be named from `from`.
    pub fn def_visible(&self, d: DefId, from: DefId) -> bool {
        let def = self.def(d);
        def.is_pub || self.is_within(from, def.module)
    }

    // ------------------------------------------------------------ lookup

    fn lookup_in(&self, m: DefId, name: &str, type_ns: bool) -> Lookup {
        let md = &self.mods[&m];
        let (exp, glob) = if type_ns {
            (&md.types, &md.glob_types)
        } else {
            (&md.values, &md.glob_values)
        };
        if let Some(b) = exp.get(name) {
            return Ok(Some(b.clone()));
        }
        if let Some(v) = glob.get(name) {
            let mut defs: Vec<DefId> = v.iter().map(|b| b.def).collect();
            defs.sort();
            defs.dedup();
            if defs.len() == 1 {
                return Ok(Some(v[0].clone()));
            }
            return Err(v.clone());
        }
        Ok(None)
    }

    /// Members of module `m` as seen from `from` (used for qualified paths
    /// and imports): explicit and glob bindings, privacy checked by caller.
    pub fn member(&self, m: DefId, name: &str) -> (Lookup, Lookup) {
        (
            self.lookup_in(m, name, true),
            self.lookup_in(m, name, false),
        )
    }

    /// Unqualified lookup in module `m`: the module's own items and imports,
    /// then glob imports, then the prelude, then primitive types.
    pub fn lookup(&self, m: DefId, name: &str, type_ns: bool) -> Lookup {
        if let Some(b) = self.lookup_in(m, name, type_ns)? {
            return Ok(Some(b));
        }
        if self.mods[&m].package_root != self.core_root {
            if let Ok(Some(b)) = self.lookup_in(self.core_root, name, type_ns) {
                if b.is_pub {
                    return Ok(Some(b));
                }
            }
        }
        if type_ns {
            if let Some(&d) = self.prims.get(name) {
                return Ok(Some(Binding {
                    def: d,
                    is_pub: true,
                    file: 0,
                    span: (0, 0),
                }));
            }
        }
        Ok(None)
    }

    // ------------------------------------------------------------ imports

    /// Resolve every `use`, iterating to a fixed point so imports may depend
    /// on other imports and globs in any order.
    pub fn resolve_imports(&mut self, diags: &mut Diags) {
        let mut pending: Vec<Import> = std::mem::take(&mut self.imports);
        loop {
            let mut progress = false;
            let mut still = Vec::new();
            for imp in pending {
                match self.try_import(&imp) {
                    Ok(true) => progress = true,
                    Ok(false) => still.push(imp),
                    Err((code, msg)) => {
                        diags.error(code, imp.file, imp.span, msg);
                        progress = true;
                    }
                }
            }
            pending = still;
            if self.refresh_globs() {
                progress = true;
            }
            if !progress {
                break;
            }
        }
        for imp in pending {
            let msg = match self.import_failure(&imp) {
                Some(m) => m,
                None => format!("unresolved import `{}`", imp.path.join(".")),
            };
            diags.error("R002", imp.file, imp.span, msg);
        }
        for g in self.globs.clone() {
            let _ = (g.file, g.span);
        }
    }

    /// Resolve the leading segments of an import path to a module or enum.
    /// Ok(None): not resolvable yet (may appear later).
    fn import_container(
        &self,
        imp: &Import,
        segs: &[String],
    ) -> Result<Option<DefId>, (&'static str, String)> {
        let mut cur: DefId;
        let first = &segs[0];
        if first == "self" {
            cur = imp.module;
        } else {
            let root = self.mods[&imp.module].package_root;
            match self.lookup_in(root, first, true) {
                Ok(Some(b)) if matches!(self.def(b.def).kind, DefKind::Mod | DefKind::Enum) => {
                    cur = b.def
                }
                Ok(Some(b)) => {
                    return Err((
                        "R003",
                        format!(
                            "`{first}` is a {}, not a module",
                            self.def(b.def).kind.describe()
                        ),
                    ))
                }
                Err(_) => return Err(("R005", format!("`{first}` is ambiguous"))),
                Ok(None) if first == "core" => cur = self.core_root,
                Ok(None) => return Ok(None),
            }
        }
        for s in &segs[1..] {
            if self.def(cur).kind == DefKind::Enum {
                return Err((
                    "R003",
                    format!("enum `{}` has no nested items", self.def(cur).name),
                ));
            }
            match self.lookup_in(cur, s, true) {
                Ok(Some(b)) => {
                    if !self.binding_visible(&b, cur, imp.module) {
                        return Err(("R004", format!("`{s}` is private")));
                    }
                    match self.def(b.def).kind {
                        DefKind::Mod | DefKind::Enum => cur = b.def,
                        k => {
                            return Err((
                                "R003",
                                format!("`{s}` is a {}, not a module", k.describe()),
                            ))
                        }
                    }
                }
                Err(_) => return Err(("R005", format!("`{s}` is ambiguous"))),
                Ok(None) => return Ok(None),
            }
        }
        Ok(Some(cur))
    }

    fn try_import(&mut self, imp: &Import) -> Result<bool, (&'static str, String)> {
        match &imp.kind {
            ImportKind::Glob => {
                let Some(target) = self.import_container(imp, &imp.path)? else {
                    return Ok(false);
                };
                self.globs.push(Glob {
                    module: imp.module,
                    target,
                    is_pub: imp.is_pub,
                    file: imp.file,
                    span: imp.span,
                });
                Ok(true)
            }
            ImportKind::Single(alias) => {
                let (last, prefix) = imp.path.split_last().unwrap();
                let found = if prefix.is_empty() {
                    if last == "self" {
                        return Err(("R003", "`use self` imports nothing".to_string()));
                    }
                    // `use x`: a top-level item of the package (or core).
                    let root = self.mods[&imp.module].package_root;
                    let mut per = self.ns_of(root, last)?;
                    if per.ty.is_none() && per.value.is_none() && last == "core" {
                        per.ty = Some(Binding {
                            def: self.core_root,
                            is_pub: true,
                            file: imp.file,
                            span: imp.span,
                        });
                    }
                    if per.ty.is_none() && per.value.is_none() {
                        return Ok(false);
                    }
                    (root, per)
                } else {
                    let Some(c) = self.import_container(imp, prefix)? else {
                        return Ok(false);
                    };
                    if self.def(c).kind == DefKind::Enum {
                        let v = self.variants[&c]
                            .iter()
                            .copied()
                            .find(|&v| self.def(v).name == *last);
                        match v {
                            Some(v) => {
                                let b = Binding {
                                    def: v,
                                    is_pub: true,
                                    file: imp.file,
                                    span: imp.span,
                                };
                                (
                                    c,
                                    PerNs {
                                        ty: None,
                                        value: Some(b),
                                    },
                                )
                            }
                            None => {
                                return Err((
                                    "R002",
                                    format!("enum `{}` has no variant `{last}`", self.def(c).name),
                                ))
                            }
                        }
                    } else {
                        let per = self.ns_of(c, last)?;
                        if per.ty.is_none() && per.value.is_none() {
                            return Ok(false);
                        }
                        (c, per)
                    }
                };
                let (holder, per) = found;
                let mut any_visible = false;
                let mut any = false;
                for (b, type_ns) in [(per.ty, true), (per.value, false)] {
                    let Some(b) = b else { continue };
                    any = true;
                    let holder_mod = if self.def(holder).kind == DefKind::Enum {
                        self.def(holder).module
                    } else {
                        holder
                    };
                    if self.def(holder).kind != DefKind::Enum
                        && holder != imp.module
                        && !self.binding_visible(&b, holder_mod, imp.module)
                    {
                        continue;
                    }
                    any_visible = true;
                    let nb = Binding {
                        def: b.def,
                        is_pub: imp.is_pub,
                        file: imp.file,
                        span: imp.span,
                    };
                    let md = self.mods.get_mut(&imp.module).unwrap();
                    let ns = if type_ns {
                        &mut md.types
                    } else {
                        &mut md.values
                    };
                    if let Some(prev) = ns.get(alias) {
                        if prev.def != nb.def {
                            return Err((
                                "R001",
                                format!("`{alias}` is defined more than once in this module"),
                            ));
                        }
                    } else {
                        ns.insert(alias.clone(), nb);
                    }
                }
                if any && !any_visible {
                    return Err(("R004", format!("`{}` is private", imp.path.join("."))));
                }
                Ok(true)
            }
        }
    }

    fn ns_of(&self, m: DefId, name: &str) -> Result<PerNs, (&'static str, String)> {
        let (t, v) = self.member(m, name);
        let amb = |_| {
            (
                "R005",
                format!("`{name}` is ambiguous (several glob imports provide it)"),
            )
        };
        Ok(PerNs {
            ty: t.map_err(amb)?,
            value: v.map_err(amb)?,
        })
    }

    fn import_failure(&self, imp: &Import) -> Option<String> {
        let first = &imp.path[0];
        if first == "std" {
            return Some(format!(
                "unresolved import `{}`: the standard library does not exist yet",
                imp.path.join(".")
            ));
        }
        None
    }

    /// Recompute glob bindings. Returns true if anything changed.
    fn refresh_globs(&mut self) -> bool {
        let mut changed = false;
        for g in self.globs.clone() {
            let mut add: Vec<(String, Binding, bool)> = Vec::new();
            if self.def(g.target).kind == DefKind::Enum {
                for &v in &self.variants[&g.target] {
                    add.push((self.def(v).name.clone(), self.mkb(v, &g), false));
                }
            } else {
                let td = &self.mods[&g.target];
                for (type_ns, exp, glob) in [
                    (true, &td.types, &td.glob_types),
                    (false, &td.values, &td.glob_values),
                ] {
                    for (n, b) in exp {
                        if self.binding_visible(b, g.target, g.module) {
                            add.push((n.clone(), self.mkb(b.def, &g), type_ns));
                        }
                    }
                    for (n, bs) in glob {
                        for b in bs {
                            if self.binding_visible(b, g.target, g.module) {
                                add.push((n.clone(), self.mkb(b.def, &g), type_ns));
                            }
                        }
                    }
                }
            }
            let md = self.mods.get_mut(&g.module).unwrap();
            for (n, b, type_ns) in add {
                let gl = if type_ns {
                    &mut md.glob_types
                } else {
                    &mut md.glob_values
                };
                let e = gl.entry(n).or_default();
                if !e.iter().any(|x| x.def == b.def) {
                    e.push(b);
                    changed = true;
                }
            }
        }
        changed
    }

    fn mkb(&self, d: DefId, g: &Glob) -> Binding {
        Binding {
            def: d,
            is_pub: g.is_pub,
            file: g.file,
            span: g.span,
        }
    }
}

fn flatten_use(tree: &UseTree, prefix: &[String], out: &mut Vec<(Vec<String>, ImportKind)>) {
    let join = |p: &[String]| {
        let mut v = prefix.to_vec();
        v.extend_from_slice(p);
        v
    };
    match tree {
        UseTree::Path { path, alias } => {
            let full = join(path);
            let name = alias
                .clone()
                .unwrap_or_else(|| full.last().cloned().unwrap_or_default());
            out.push((full, ImportKind::Single(name)));
        }
        UseTree::Group { prefix: p, trees } => {
            let full = join(p);
            for t in trees {
                flatten_use(t, &full, out);
            }
        }
        UseTree::Glob { prefix: p } => out.push((join(p), ImportKind::Glob)),
    }
}

/// Load every file the package needs, following body-less `mod` items.
pub fn load_package(
    loader: &dyn crate::source::Loader,
    root_file: &str,
    diags: &mut Diags,
) -> (Vec<SourceFile>, HashMap<(FileId, u32), FileId>) {
    let mut files = vec![SourceFile::parse(
        "<core>".to_string(),
        crate::source::PRELUDE_SRC.as_bytes().to_vec(),
    )];
    let mut map = HashMap::new();
    let Some(src) = loader.read(root_file) else {
        diags.error(
            "R013",
            1,
            (0, 0),
            format!("cannot read the package root file `{root_file}`"),
        );
        files.push(SourceFile::parse(root_file.to_string(), Vec::new()));
        return (files, map);
    };
    files.push(SourceFile::parse(root_file.to_string(), src));
    let root_dir: Vec<String> = match root_file.rsplit_once('/') {
        Some((d, _)) => d.split('/').map(String::from).collect(),
        None => Vec::new(),
    };
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(root_file.to_string());
    let mut work: Vec<(FileId, Vec<String>)> = vec![(1, root_dir)];
    while let Some((fid, dir)) = work.pop() {
        let mut found: Vec<(u32, String, Vec<String>, Span)> = Vec::new();
        collect_mod_decls(&files[fid as usize].module.items, &dir, &mut found);
        for (start, name, d, span) in found {
            let base = if d.is_empty() {
                String::new()
            } else {
                format!("{}/", d.join("/"))
            };
            let a = format!("{base}{name}.peg");
            let b = format!("{base}{name}/mod.peg");
            let ra = loader.read(&a);
            let rb = loader.read(&b);
            let (path, src) = match (ra, rb) {
                (Some(_), Some(_)) => {
                    diags.error(
                        "R014",
                        fid,
                        span,
                        format!("module `{name}` is ambiguous: both `{a}` and `{b}` exist"),
                    );
                    continue;
                }
                (Some(s), None) => (a, s),
                (None, Some(s)) => (b, s),
                (None, None) => {
                    diags.error(
                        "R013",
                        fid,
                        span,
                        format!("module `{name}` not found: expected `{a}` or `{b}`"),
                    );
                    continue;
                }
            };
            if !seen.insert(path.clone()) {
                diags.error(
                    "R014",
                    fid,
                    span,
                    format!("file `{path}` is loaded as a module twice"),
                );
                continue;
            }
            files.push(SourceFile::parse(path, src));
            let nid = (files.len() - 1) as FileId;
            map.insert((fid, start), nid);
            let mut nd = d.clone();
            nd.push(name);
            work.push((nid, nd));
        }
    }
    (files, map)
}

fn collect_mod_decls(
    items: &[Item],
    dir: &[String],
    out: &mut Vec<(u32, String, Vec<String>, Span)>,
) {
    for it in items {
        if let ItemKind::Mod { name, items: body } = &it.kind {
            match body {
                None => out.push((it.span.0, name.clone(), dir.to_vec(), it.span)),
                Some(inner) => {
                    let mut d = dir.to_vec();
                    d.push(name.clone());
                    collect_mod_decls(inner, &d, out);
                }
            }
        }
    }
}
