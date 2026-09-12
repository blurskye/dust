//! Canonical, conservative source dependency checks. This is not rustc's name
//! resolver: cfg branches are unioned and aliases in nested blocks are unioned
//! within their real module. External procedural-macro expansion is not inspected.
use super::ArchitectureError;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};
#[cfg(test)]
mod tests;
use syn::{
    Item, UseTree,
    visit::{self, Visit},
};

#[derive(Default)]
struct Module {
    aliases: BTreeMap<String, Vec<Vec<String>>>,
    globs: Vec<Vec<String>>,
    definitions: BTreeSet<String>,
}
struct Wildcard {
    module: String,
    file: PathBuf,
    reexport: bool,
}
#[derive(Default)]
struct Graph {
    modules: BTreeMap<String, Module>,
    references: Vec<(String, PathBuf, Vec<String>)>,
    wildcards: Vec<Wildcard>,
    self_aliases: BTreeSet<String>,
}

type LookupKey = (String, Vec<String>);
#[derive(Default)]
struct Lookup {
    active: BTreeSet<LookupKey>,
    completed: BTreeMap<LookupKey, BTreeSet<String>>,
    steps: usize,
    cycles: usize,
}

fn failure(path: &Path, detail: impl Into<String>) -> ArchitectureError {
    ArchitectureError::Dependency {
        path: path.to_owned(),
        detail: detail.into(),
    }
}
fn production(attrs: &[syn::Attribute]) -> bool {
    fn test_only(meta: &syn::Meta) -> bool {
        match meta {
            syn::Meta::Path(path) => path.is_ident("test"),
            syn::Meta::List(list) => {
                let Ok(parts) = list.parse_args_with(
                    syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
                ) else {
                    return false;
                };
                if list.path.is_ident("all") {
                    parts.iter().any(test_only)
                } else if list.path.is_ident("any") {
                    !parts.is_empty() && parts.iter().all(test_only)
                } else {
                    false
                }
            }
            _ => false,
        }
    }
    !attrs.iter().any(|attr| {
        attr.path().is_ident("test")
            || attr.path().is_ident("cfg")
                && attr
                    .parse_args::<syn::Meta>()
                    .is_ok_and(|meta| test_only(&meta))
    })
}
fn attrs(item: &Item) -> &[syn::Attribute] {
    match item {
        Item::Const(v) => &v.attrs,
        Item::Enum(v) => &v.attrs,
        Item::ExternCrate(v) => &v.attrs,
        Item::Fn(v) => &v.attrs,
        Item::ForeignMod(v) => &v.attrs,
        Item::Impl(v) => &v.attrs,
        Item::Macro(v) => &v.attrs,
        Item::Mod(v) => &v.attrs,
        Item::Static(v) => &v.attrs,
        Item::Struct(v) => &v.attrs,
        Item::Trait(v) => &v.attrs,
        Item::TraitAlias(v) => &v.attrs,
        Item::Type(v) => &v.attrs,
        Item::Union(v) => &v.attrs,
        Item::Use(v) => &v.attrs,
        _ => &[],
    }
}
fn path_parts(path: &syn::Path) -> Vec<String> {
    path.segments
        .iter()
        .map(|part| part.ident.to_string())
        .collect()
}
fn flatten(tree: &UseTree, prefix: Vec<String>, aliases: &mut Vec<(Option<String>, Vec<String>)>) {
    match tree {
        UseTree::Path(part) => {
            let mut prefix = prefix;
            prefix.push(part.ident.to_string());
            flatten(&part.tree, prefix, aliases);
        }
        UseTree::Name(name) => {
            let mut target = prefix;
            if name.ident != "self" {
                target.push(name.ident.to_string());
            }
            if let Some(name) = target.last().cloned() {
                aliases.push((Some(name), target));
            }
        }
        UseTree::Rename(rename) => {
            let mut target = prefix;
            if rename.ident != "self" {
                target.push(rename.ident.to_string());
            }
            aliases.push((Some(rename.rename.to_string()), target));
        }
        UseTree::Group(group) => {
            for item in &group.items {
                flatten(item, prefix.clone(), aliases);
            }
        }
        UseTree::Glob(_) => aliases.push((None, prefix)),
    }
}
struct References {
    nested_modules: Vec<syn::ItemMod>,
    paths: Vec<Vec<String>>,
    uses: Vec<(Option<String>, Vec<String>, bool)>,
    aliases: Vec<(String, Vec<Vec<String>>)>,
}
impl References {
    fn new() -> Self {
        Self {
            nested_modules: Vec::new(),
            paths: Vec::new(),
            uses: Vec::new(),
            aliases: Vec::new(),
        }
    }
}
impl<'ast> Visit<'ast> for References {
    fn visit_item(&mut self, item: &'ast Item) {
        if production(attrs(item)) {
            visit::visit_item(self, item);
        }
    }
    fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
        self.nested_modules.push(module.clone());
    }
    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        if production(&item.attrs) {
            visit::visit_impl_item_fn(self, item);
        }
    }
    fn visit_trait_item_fn(&mut self, item: &'ast syn::TraitItemFn) {
        if production(&item.attrs) {
            visit::visit_trait_item_fn(self, item);
        }
    }
    fn visit_field(&mut self, field: &'ast syn::Field) {
        if production(&field.attrs) {
            visit::visit_field(self, field);
        }
    }
    fn visit_attribute(&mut self, _: &'ast syn::Attribute) {}
    fn visit_path(&mut self, path: &'ast syn::Path) {
        self.paths.push(path_parts(path));
        visit::visit_path(self, path);
    }
    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        let mut uses = Vec::new();
        flatten(&item.tree, Vec::new(), &mut uses);
        let reexport = !matches!(item.vis, syn::Visibility::Inherited);
        self.uses.extend(
            uses.into_iter()
                .map(|(alias, target)| (alias, target, reexport)),
        );
    }
    fn visit_item_type(&mut self, item: &'ast syn::ItemType) {
        let mut refs = References::new();
        refs.visit_type(&item.ty);
        self.aliases
            .push((item.ident.to_string(), refs.paths.clone()));
        self.paths.extend(refs.paths);
    }
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        self.paths.push(path_parts(&mac.path));
        // Inspect identifiers in macro arguments as tokens, never comments or
        // string contents. The macro's generated expansion remains outside scope.
        if let Ok(tokens) = syn::parse2::<TokenPaths>(mac.tokens.clone()) {
            self.paths.extend(tokens.0);
        }
    }
}
struct TokenPaths(Vec<Vec<String>>);
impl syn::parse::Parse for TokenPaths {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        use syn::ext::IdentExt;
        let mut paths = Vec::new();
        while !input.is_empty() {
            if input.peek(syn::token::Brace) {
                let inside;
                syn::braced!(inside in input);
                paths.extend(inside.parse::<Self>()?.0);
            } else if input.peek(syn::token::Bracket) {
                let inside;
                syn::bracketed!(inside in input);
                paths.extend(inside.parse::<Self>()?.0);
            } else if input.peek(syn::token::Paren) {
                let inside;
                syn::parenthesized!(inside in input);
                paths.extend(inside.parse::<Self>()?.0);
            } else if input.fork().call(syn::Ident::parse_any).is_ok() {
                let mut path = vec![input.call(syn::Ident::parse_any)?.to_string()];
                while input.peek(syn::Token![::]) {
                    input.parse::<syn::Token![::]>()?;
                    if input.fork().call(syn::Ident::parse_any).is_err() {
                        break;
                    }
                    path.push(input.call(syn::Ident::parse_any)?.to_string());
                }
                paths.push(path);
            } else {
                input.step(|cursor| {
                    cursor
                        .token_tree()
                        .map(|(_, rest)| ((), rest))
                        .ok_or_else(|| cursor.error("unaccounted macro token"))
                })?;
            }
        }
        Ok(Self(paths))
    }
}
fn clean(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            part => out.push(part.as_os_str()),
        }
    }
    out
}
impl Graph {
    fn load(
        &mut self,
        name: String,
        file: &Path,
        files: &BTreeMap<PathBuf, String>,
        depth: usize,
    ) -> Result<(), ArchitectureError> {
        if depth > 64 {
            return Err(failure(file, "module depth exceeded"));
        }
        let source = files.get(file).ok_or_else(|| {
            failure(
                file,
                "module target is outside the bounded source inventory",
            )
        })?;
        let ast = syn::parse_file(source)
            .map_err(|error| failure(file, format!("Rust parse: {error}")))?;
        if !production(&ast.attrs) {
            return Ok(());
        }
        let child_dir = if file
            .file_name()
            .is_some_and(|name| name == "mod.rs" || name == "lib.rs")
        {
            file.parent().unwrap().to_owned()
        } else {
            file.with_extension("")
        };
        self.items(
            name,
            file,
            (&child_dir, file.parent().unwrap()),
            &ast.items,
            files,
            depth,
        )
    }
    fn items(
        &mut self,
        name: String,
        file: &Path,
        directories: (&Path, &Path),
        items: &[Item],
        files: &BTreeMap<PathBuf, String>,
        depth: usize,
    ) -> Result<(), ArchitectureError> {
        let (child_dir, attribute_dir) = directories;
        if depth > 64 {
            return Err(failure(file, "module depth exceeded"));
        }
        if self.modules.contains_key(&name) {
            return Err(failure(file, format!("ambiguous module {name}")));
        }
        let mut module = Module::default();
        let mut children = Vec::new();
        for item in items.iter().filter(|item| production(attrs(item))) {
            if let Item::Mod(child) = item {
                children.push(child.clone());
                module.definitions.insert(child.ident.to_string());
                continue;
            }
            let definition = match item {
                Item::Struct(v) => Some(&v.ident),
                Item::Enum(v) => Some(&v.ident),
                Item::Fn(v) => Some(&v.sig.ident),
                Item::Const(v) => Some(&v.ident),
                Item::Static(v) => Some(&v.ident),
                Item::Trait(v) => Some(&v.ident),
                Item::Union(v) => Some(&v.ident),
                _ => None,
            };
            if let Some(definition) = definition {
                module.definitions.insert(definition.to_string());
            }
            if let Item::ExternCrate(external) = item {
                if name == "crate" && external.ident == "self" {
                    self.self_aliases.insert(
                        external
                            .rename
                            .as_ref()
                            .map_or(&external.ident, |(_, alias)| alias)
                            .to_string(),
                    );
                }
                let alias = external
                    .rename
                    .as_ref()
                    .map_or(&external.ident, |(_, alias)| alias)
                    .to_string();
                module
                    .aliases
                    .entry(alias)
                    .or_default()
                    .push(vec![if external.ident == "self" {
                        "crate".into()
                    } else {
                        external.ident.to_string()
                    }]);
            }
            let mut refs = References::new();
            refs.visit_item(item);
            children.extend(refs.nested_modules);
            for (alias, target, reexport) in refs.uses {
                if alias.is_none() {
                    self.wildcards.push(Wildcard {
                        module: name.clone(),
                        file: file.to_owned(),
                        reexport,
                    });
                }
                self.references
                    .push((name.clone(), file.to_owned(), target.clone()));
                if let Some(alias) = alias {
                    module.aliases.entry(alias).or_default().push(target);
                } else {
                    module.globs.push(target);
                }
            }
            for (alias, targets) in refs.aliases {
                module.aliases.entry(alias).or_default().extend(targets);
            }
            self.references.extend(
                refs.paths
                    .into_iter()
                    .map(|path| (name.clone(), file.to_owned(), path)),
            );
        }
        self.modules.insert(name.clone(), module);
        for child in children {
            let child_name = format!("{name}::{}", child.ident);
            if child
                .attrs
                .iter()
                .any(|attr| attr.path().is_ident("cfg_attr"))
            {
                return Err(failure(
                    file,
                    "conditional module attributes require an explicit audited module inventory",
                ));
            }
            if let Some((_, items)) = &child.content {
                let directory = child_dir.join(child.ident.to_string());
                self.items(
                    child_name,
                    file,
                    (&directory, &directory),
                    items,
                    files,
                    depth + 1,
                )?;
            } else {
                let explicit = child
                    .attrs
                    .iter()
                    .find(|attr| attr.path().is_ident("path"))
                    .and_then(|attr| match &attr.meta {
                        syn::Meta::NameValue(value) => match &value.value {
                            syn::Expr::Lit(syn::ExprLit {
                                lit: syn::Lit::Str(value),
                                ..
                            }) => Some(value.value()),
                            _ => None,
                        },
                        _ => None,
                    });
                if explicit.is_none() && child.attrs.iter().any(|attr| attr.path().is_ident("path"))
                {
                    return Err(failure(file, "unaccounted module path attribute"));
                }
                let target = if let Some(explicit) = explicit {
                    clean(&attribute_dir.join(explicit))
                } else {
                    let leaf = child_dir.join(format!("{}.rs", child.ident));
                    if files.contains_key(&leaf) {
                        leaf
                    } else {
                        child_dir.join(child.ident.to_string()).join("mod.rs")
                    }
                };
                self.load(child_name, &target, files, depth + 1)?;
            }
        }
        Ok(())
    }
    fn resolve(&self, module: &str, parts: &[String], seen: &mut Lookup) -> BTreeSet<String> {
        if parts.is_empty() {
            return BTreeSet::from([module.to_owned()]);
        }
        let key = (module.to_owned(), parts.to_vec());
        if let Some(found) = seen.completed.get(&key) {
            return found.clone();
        }
        seen.steps += 1;
        if seen.steps > 8192 || seen.active.len() > 128 {
            return BTreeSet::from(["!resolution-limit".into()]);
        }
        if !seen.active.insert(key.clone()) {
            seen.cycles += 1;
            return BTreeSet::new();
        }
        let cycles = seen.cycles;
        let found = self.lookup(module, parts, seen);
        seen.active.remove(&key);
        // A result reached through a cut cycle may be incomplete in a different
        // traversal. Never cache it as a negative (or as a complete positive).
        if seen.cycles == cycles {
            seen.completed.insert(key, found.clone());
        }
        found
    }
    fn lookup(&self, module: &str, parts: &[String], seen: &mut Lookup) -> BTreeSet<String> {
        let first = &parts[0];
        if first == "crate" {
            return self.absolute("crate", &parts[1..], seen);
        }
        if first == "self" {
            return self.absolute(module, &parts[1..], seen);
        }
        if first == "super" {
            return self.resolve(
                module
                    .rsplit_once("::")
                    .map_or("crate", |(parent, _)| parent),
                &parts[1..],
                seen,
            );
        }
        let Some(data) = self.modules.get(module) else {
            return BTreeSet::from([format!("{module}::{}", parts.join("::"))]);
        };
        if let Some(targets) = data.aliases.get(first) {
            let mut found = BTreeSet::new();
            for target in targets {
                for target in self.resolve(module, target, seen) {
                    found.extend(self.absolute(&target, &parts[1..], seen));
                }
            }
            return found;
        }
        if data.definitions.contains(first) {
            return self.absolute(&format!("{module}::{first}"), &parts[1..], seen);
        }
        if self.self_aliases.contains(first) {
            return self.absolute("crate", &parts[1..], seen);
        }
        let mut found = BTreeSet::new();
        for glob in &data.globs {
            for target in self.resolve(module, glob, seen) {
                if self.modules.contains_key(&target) {
                    found.extend(self.resolve(&target, parts, seen));
                }
            }
        }
        found
    }
    fn absolute(&self, prefix: &str, rest: &[String], seen: &mut Lookup) -> BTreeSet<String> {
        if rest.is_empty() {
            return BTreeSet::from([prefix.to_owned()]);
        }
        if self.modules.contains_key(prefix) {
            self.resolve(prefix, rest, seen)
        } else {
            BTreeSet::from([format!("{prefix}::{}", rest.join("::"))])
        }
    }
}

pub(super) fn audit(root: &Path, paths: &[PathBuf]) -> Result<(), ArchitectureError> {
    let files = paths
        .iter()
        .map(|path| std::fs::read_to_string(path).map(|source| (clean(path), source)))
        .collect::<Result<BTreeMap<_, _>, _>>()
        .map_err(ArchitectureError::Io)?;
    let mut graph = Graph::default();
    graph.load("crate".into(), &clean(&root.join("src/lib.rs")), &files, 0)?;
    let binary = clean(&root.join("src/main.rs"));
    if files.contains_key(&binary) {
        graph.load("binary".into(), &binary, &files, 0)?;
    }
    let command_root = clean(&root.join("src/editor/session/commands"));
    let config_root = clean(&root.join("src/config"));
    let files_root = clean(&root.join("src/files"));
    let workspace_root = clean(&root.join("src/workspace"));
    let dispatch_root = clean(&root.join("src/editor/session/dispatch"));
    if let Some(Wildcard { module, file, .. }) = graph.wildcards.iter().find(|wildcard| {
        let module = &wildcard.module;
        let file = &wildcard.file;
        [
            "configuration",
            "documents",
            "input",
            "notifications",
            "syntax",
            "windows",
            "workspace",
            "persistence",
            "search",
            "explorer",
            "workspace_edits",
            "lsp_edits",
            "plugin_edits",
            "diagnostics",
            "tasks",
            "terminal",
            "debugger",
            "git",
            "recovery",
            "tooling",
            "lsp",
            "extensions",
        ]
        .iter()
        .any(|owner| {
            module == &format!("crate::editor::session::{owner}")
                || module.starts_with(&format!("crate::editor::session::{owner}::"))
        }) || file.starts_with(&command_root)
            || file.starts_with(&config_root)
            || module == "crate::config"
            || module.starts_with("crate::config::")
            || file.starts_with(&files_root)
            || module == "crate::files"
            || module.starts_with("crate::files::")
            || file.starts_with(&workspace_root)
            || module == "crate::workspace"
            || module.starts_with("crate::workspace::")
            || file.starts_with(&dispatch_root)
            || module == "crate::editor::session::dispatch"
            || module.starts_with("crate::editor::session::dispatch::")
    }) {
        return Err(failure(
            file,
            format!(
                "migrated owner, configuration, filesystem, workspace, or composition module {module} must name its imports and exports explicitly ({} production wildcards remain globally)",
                graph.wildcards.len()
            ),
        ));
    }
    let reexports = graph
        .wildcards
        .iter()
        .filter(|wildcard| wildcard.reexport)
        .count();
    if let Some(wildcard) = graph.wildcards.iter().find(|wildcard| wildcard.reexport) {
        return Err(failure(
            &wildcard.file,
            format!(
                "production wildcard reexport in {} must name every export explicitly ({reexports} wildcard reexports; {} production wildcards remain globally)",
                wildcard.module,
                graph.wildcards.len()
            ),
        ));
    }
    for (module, file, reference) in &graph.references {
        let service = [
            "files",
            "workspace",
            "process",
            "language",
            "debugger",
            "tools",
            "extensions",
            "persistence",
            "telemetry",
        ]
        .iter()
        .any(|owner| {
            module == &format!("crate::{owner}") || module.starts_with(&format!("crate::{owner}::"))
        });
        if !service {
            continue;
        }
        for target in graph.resolve(module, reference, &mut Lookup::default()) {
            if target.starts_with("!resolution-limit") {
                return Err(failure(file, "canonical alias resolution budget exceeded"));
            }
            let editor_data = target.starts_with("crate::editor::text::")
                || target == "crate::editor::text"
                || target.starts_with("crate::editor::state::id::")
                || module.starts_with("crate::extensions")
                    && (target.starts_with("crate::editor::input::key::")
                        || target.starts_with("crate::editor::input::context::"));
            if target.starts_with("crate::editor") && !editor_data
                || ["ui", "testing", "architecture", "api", "release"]
                    .iter()
                    .any(|owner| {
                        target == format!("crate::{owner}")
                            || target.starts_with(&format!("crate::{owner}::"))
                    })
            {
                return Err(failure(
                    file,
                    format!("{module} depends on forbidden authority {target}"),
                ));
            }
        }
    }
    Ok(())
}
