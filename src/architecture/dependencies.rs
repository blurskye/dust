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
    mutable_references: Vec<(String, PathBuf, Vec<String>)>,
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
    mutable_paths: Vec<Vec<String>>,
    uses: Vec<(Option<String>, Vec<String>, bool)>,
    aliases: Vec<(String, Vec<Vec<String>>)>,
}
impl References {
    fn new() -> Self {
        Self {
            nested_modules: Vec::new(),
            paths: Vec::new(),
            mutable_paths: Vec::new(),
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
    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        if item.trait_.as_ref().is_some_and(|(_, path, _)| {
            path.segments.last().is_some_and(|segment| {
                matches!(
                    segment.ident.to_string().as_str(),
                    "AsMut" | "BorrowMut" | "Deref" | "DerefMut" | "Index" | "IndexMut"
                )
            })
        }) {
            let mut refs = References::new();
            visit::visit_item_impl(&mut refs, item);
            self.mutable_paths.extend(refs.paths);
        }
        visit::visit_item_impl(self, item);
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
    fn visit_type_reference(&mut self, reference: &'ast syn::TypeReference) {
        if reference.mutability.is_some() {
            let mut refs = References::new();
            refs.visit_type(&reference.elem);
            self.mutable_paths.extend(refs.paths);
        }
        visit::visit_type_reference(self, reference);
    }
    fn visit_type_ptr(&mut self, pointer: &'ast syn::TypePtr) {
        if pointer.mutability.is_some() {
            let mut refs = References::new();
            refs.visit_type(&pointer.elem);
            self.mutable_paths.extend(refs.paths);
        }
        visit::visit_type_ptr(self, pointer);
    }
    fn visit_type_path(&mut self, path: &'ast syn::TypePath) {
        if path.path.segments.last().is_some_and(|segment| {
            matches!(
                segment.ident.to_string().as_str(),
                "AsMut"
                    | "BorrowMut"
                    | "Cell"
                    | "IndexMut"
                    | "Mutex"
                    | "MutexGuard"
                    | "RefCell"
                    | "RefMut"
                    | "RwLock"
                    | "RwLockWriteGuard"
                    | "UnsafeCell"
            )
        }) {
            let mut refs = References::new();
            visit::visit_type_path(&mut refs, path);
            self.mutable_paths.extend(refs.paths);
        }
        visit::visit_type_path(self, path);
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
            if tokens.0.iter().any(|path| path == &["mut"]) {
                self.mutable_paths.extend(
                    tokens
                        .0
                        .iter()
                        .filter(|path| path.as_slice() != ["mut"])
                        .cloned(),
                );
            }
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
            self.mutable_references.extend(
                refs.mutable_paths
                    .into_iter()
                    .map(|path| (name.clone(), file.to_owned(), path)),
            );
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeterministicSource {
    Text,
    State,
    Transaction,
    Command,
    Input,
    Ui,
}

fn source_root(root: &Path, module: &str, file: &Path, logical: &str, physical: &str) -> bool {
    let source = clean(&root.join("src").join(physical));
    module == logical
        || module.starts_with(&format!("{logical}::"))
        || file == source.with_extension("rs")
        || file.starts_with(source)
}

fn deterministic_source(root: &Path, module: &str, file: &Path) -> Option<DeterministicSource> {
    [
        (
            "crate::editor::text",
            "editor/text",
            DeterministicSource::Text,
        ),
        (
            "crate::editor::state",
            "editor/state",
            DeterministicSource::State,
        ),
        (
            "crate::editor::transaction",
            "editor/transaction",
            DeterministicSource::Transaction,
        ),
        (
            "crate::editor::command",
            "editor/command",
            DeterministicSource::Command,
        ),
        (
            "crate::editor::input",
            "editor/input",
            DeterministicSource::Input,
        ),
        ("crate::ui", "ui", DeterministicSource::Ui),
    ]
    .into_iter()
    .find_map(|(logical, physical, source)| {
        source_root(root, module, file, logical, physical).then_some(source)
    })
}

fn target_in(target: &str, root: &str) -> bool {
    target == root || target.starts_with(&format!("{root}::"))
}

fn target_names(target: &str, root: &str, names: &[&str]) -> bool {
    target_in(target, root) && target.split("::").any(|part| names.contains(&part))
}

fn deterministic_dependency_allowed(source: DeterministicSource, target: &str) -> bool {
    if !target.starts_with("crate::") {
        return true;
    }
    let editor = |family| target_in(target, &format!("crate::editor::{family}"));
    let universal_limit = matches!(
        target,
        "crate::UNIVERSAL_LAYOUT_DEPTH_CEILING" | "crate::UNIVERSAL_LAYOUT_WINDOW_CEILING"
    );
    match source {
        DeterministicSource::Text => editor("text"),
        DeterministicSource::State => {
            universal_limit
                || ["text", "state", "transaction", "command"]
                    .into_iter()
                    .any(editor)
        }
        DeterministicSource::Transaction => {
            ["text", "state", "transaction", "command"]
                .into_iter()
                .any(editor)
                || [
                    "crate::workspace::FileSnapshot",
                    "crate::workspace::ProposalState",
                    "crate::workspace::WorkspaceEditProposal",
                    "crate::workspace::edit::FileSnapshot",
                    "crate::workspace::edit::ProposalState",
                    "crate::workspace::edit::WorkspaceEditProposal",
                    "crate::persistence::PersistedLayout",
                    "crate::persistence::PersistedSession",
                    "crate::persistence::model::PersistedLayout",
                    "crate::persistence::model::PersistedSession",
                ]
                .into_iter()
                .any(|allowed| target_in(target, allowed))
        }
        DeterministicSource::Command => ["text", "state", "transaction", "command"]
            .into_iter()
            .any(editor),
        DeterministicSource::Input => ["text", "state", "transaction", "command", "input"]
            .into_iter()
            .any(editor),
        DeterministicSource::Ui => {
            universal_limit
                || target_in(target, "crate::ui")
                || target_names(
                    target,
                    "crate::config",
                    &[
                        "AmbiguousWidth",
                        "BorderStyle",
                        "ColorMode",
                        "CommandLineConfig",
                        "Config",
                        "CursorShape",
                        "DashboardConfig",
                        "EditorConfig",
                        "KeyHintAnchor",
                        "LineNumbers",
                        "PromptTextConfig",
                        "ThemeAttribute",
                        "ThemeColor",
                        "ThemeConfig",
                    ],
                )
                || ["text", "state", "input"].into_iter().any(editor)
                || [
                    "crate::language::syntax::SyntaxRole",
                    "crate::workspace::DiagnosticSeverity",
                    "crate::workspace::diagnostics::DiagnosticSeverity",
                    "crate::workspace::git::GitRepositoryId",
                    "crate::workspace::git::status::GitRepositoryId",
                ]
                .into_iter()
                .any(|allowed| target_in(target, allowed))
        }
    }
}

fn ui_authority_owner(root: &Path, module: &str, file: &Path) -> Option<&'static str> {
    [
        ("crate::ui::render", "ui/render", "render"),
        ("crate::ui::window", "ui/window", "window"),
        ("crate::ui::terminal", "ui/terminal", "terminal"),
    ]
    .into_iter()
    .find_map(|(logical, physical, owner)| {
        source_root(root, module, file, logical, physical).then_some(owner)
    })
}

fn ui_mutable_authority(target: &str) -> Option<&'static str> {
    [
        ("EditorState", "editor-state"),
        ("UiState", "render"),
        ("WindowGraph", "window"),
    ]
    .into_iter()
    .find_map(|(kind, owner)| target.ends_with(&format!("::{kind}")).then_some(owner))
    .or_else(|| {
        target_names(
            target,
            "crate::config",
            &["Config", "ResolvedConfiguration"],
        )
        .then_some("configuration")
    })
}

const SESSION_OWNERS: &[(&str, &str, &str)] = &[
    ("windows", "WindowSession", "windows"),
    ("input", "InputSession", "input"),
    ("notifications", "NoticeSession", "notifications"),
    ("syntax", "SyntaxSession", "syntax"),
    ("configuration", "ConfigurationSession", "configuration"),
    ("documents", "DocumentSession", "documents"),
    ("workspace", "WorkspaceSession", "workspace"),
    ("persistence", "PersistenceSession", "persistence"),
    ("search", "SearchSession", "search"),
    ("explorer", "ExplorerSession", "explorer"),
    ("workspace_edits", "WorkspaceEditSession", "workspace_edits"),
    ("diagnostics", "DiagnosticSession", "diagnostics"),
    ("tasks", "TaskSession", "tasks"),
    ("terminal", "TerminalSession", "terminal"),
    ("debugger", "DebugSession", "debugger"),
    ("git", "GitSession", "git"),
    ("recovery", "RecoverySession", "recovery"),
    ("tooling", "ToolingSession", "tooling"),
    ("lsp", "LspSession", "lsp"),
    ("extensions", "ExtensionSession", "extensions"),
    ("lsp_edits", "LspEditSources", "lsp"),
    ("plugin_edits", "PluginEditSources", "extensions"),
];

fn session_owner(root: &Path, module: &str, file: &Path) -> Option<&'static str> {
    let directory = clean(&root.join("src/editor/session"));
    SESSION_OWNERS.iter().find_map(|(source, _, owner)| {
        let logical = format!("crate::editor::session::{source}");
        let physical = directory.join(source);
        (module == logical
            || module.starts_with(&format!("{logical}::"))
            || file == physical.with_extension("rs")
            || file.starts_with(physical))
        .then_some(*owner)
    })
}

fn mutable_authority_owner(target: &str) -> Option<&'static str> {
    SESSION_OWNERS
        .iter()
        .find_map(|(source, kind, owner)| {
            (target.starts_with(&format!("crate::editor::session::{source}::"))
                && target.ends_with(&format!("::{kind}")))
            .then_some(*owner)
        })
        .or_else(|| {
            [
                ("EditorState", "editor-state"),
                ("UiState", "ui-state"),
                ("WindowGraph", "windows"),
                ("CommandRegistry", "command-registry"),
                ("ResolvedConfiguration", "configuration"),
            ]
            .into_iter()
            .find_map(|(kind, owner)| {
                (target.starts_with("crate::") && target.ends_with(&format!("::{kind}")))
                    .then_some(owner)
            })
        })
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
    if let Some(wildcard) = graph.wildcards.first() {
        let detail = if wildcard.reexport {
            let reexports = graph
                .wildcards
                .iter()
                .filter(|wildcard| wildcard.reexport)
                .count();
            format!(
                "production wildcard reexport in {} must name every export explicitly ({reexports} wildcard reexports; {} production wildcards remain globally)",
                wildcard.module,
                graph.wildcards.len()
            )
        } else {
            format!(
                "production module {} must name its imports and exports explicitly; this global policy includes configuration, filesystem, editor command/state/transaction, workspace, extension host, language/LSP, tools, UI render/window, migrated owners, and every other production module ({} production wildcards remain globally)",
                wildcard.module,
                graph.wildcards.len()
            )
        };
        return Err(failure(&wildcard.file, detail));
    }
    for (module, file, reference) in &graph.mutable_references {
        let session_owner = session_owner(root, module, file);
        let ui_owner = (deterministic_source(root, module, file) == Some(DeterministicSource::Ui))
            .then(|| ui_authority_owner(root, module, file).unwrap_or("ui"));
        if session_owner.is_none() && ui_owner.is_none() {
            continue;
        }
        for target in graph.resolve(module, reference, &mut Lookup::default()) {
            if target.starts_with("!resolution-limit") {
                return Err(failure(file, "canonical alias resolution budget exceeded"));
            }
            if let Some(source_owner) = session_owner {
                if let Some(target_owner) = mutable_authority_owner(&target) {
                    if target_owner != source_owner {
                        return Err(failure(
                            file,
                            format!(
                                "session owner {module} must not receive or expose mutable authority {target} owned by {target_owner}"
                            ),
                        ));
                    }
                }
            }
            if let Some(source_owner) = ui_owner {
                if let Some(target_owner) = ui_mutable_authority(&target) {
                    if target_owner != source_owner {
                        return Err(failure(
                            file,
                            format!(
                                "UI owner {module} must not receive or expose mutable authority {target} owned by {target_owner}"
                            ),
                        ));
                    }
                }
            }
        }
    }
    for (module, file, reference) in &graph.references {
        let Some(source) = deterministic_source(root, module, file) else {
            continue;
        };
        for target in graph.resolve(module, reference, &mut Lookup::default()) {
            if target.starts_with("!resolution-limit") {
                return Err(failure(file, "canonical alias resolution budget exceeded"));
            }
            if !deterministic_dependency_allowed(source, &target) {
                return Err(failure(
                    file,
                    format!(
                        "deterministic {source:?} module {module} depends on forbidden authority {target}"
                    ),
                ));
            }
        }
    }
    for (module, file, reference) in &graph.references {
        let service = [
            "config",
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
            let source = clean(&root.join("src").join(owner));
            module == &format!("crate::{owner}")
                || module.starts_with(&format!("crate::{owner}::"))
                || file == &source.with_extension("rs")
                || file.starts_with(source)
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
