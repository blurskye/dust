//! Neovim's highlight groups, such as `@function.method` and `Comment`: the names syntax
//! captures, language-server tokens and themes share. A theme colours them through
//! `ThemeConfig::highlight`.
use std::{
    collections::{BTreeMap, HashMap},
    sync::OnceLock,
};

use serde::Deserialize;

use crate::config::HighlightDefinition;

const DEFAULTS: &str = include_str!("../../../assets/defaults/highlights.toml");

/// Groups Dust's own surfaces draw with, registered first so each has a fixed id.
const BUILTIN: [&str; 15] = [
    "@comment",
    "@string",
    "@number",
    "@keyword",
    "@type",
    "@variable",
    "@function",
    "@property",
    "@attribute",
    "@markup.heading",
    "@markup.strong",
    "@markup.italic",
    "@markup.raw",
    "@markup.link",
    "@markup.quote",
];

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Highlight(u16);

impl Highlight {
    pub const COMMENT: Self = Self(0);
    pub const STRING: Self = Self(1);
    pub const NUMBER: Self = Self(2);
    pub const KEYWORD: Self = Self(3);
    pub const TYPE: Self = Self(4);
    pub const VARIABLE: Self = Self(5);
    pub const FUNCTION: Self = Self(6);
    pub const PROPERTY: Self = Self(7);
    pub const ATTRIBUTE: Self = Self(8);
    pub const HEADING: Self = Self(9);
    pub const STRONG: Self = Self(10);
    pub const ITALIC: Self = Self(11);
    pub const RAW: Self = Self(12);
    pub const LINK: Self = Self(13);
    pub const QUOTE: Self = Self(14);

    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        registry().ids.get(name).copied().map(Self)
    }

    /// The group for a tree-sitter capture: `function.method.call` is `@function.method.call`
    /// when that is a known group, otherwise the nearest parent that is.
    #[must_use]
    pub fn for_capture(capture: &str) -> Option<Self> {
        let mut name = format!("@{capture}");
        loop {
            if let Some(group) = Self::named(&name) {
                return Some(group);
            }
            let parent = name.rsplit_once('.')?.0.len();
            name.truncate(parent);
        }
    }

    /// The group for a semantic token type, as Neovim names it.
    #[must_use]
    pub fn for_semantic_token(kind: &str) -> Option<Self> {
        Self::named(&format!("@lsp.type.{kind}"))
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        &registry().names[usize::from(self.0)]
    }

    #[must_use]
    pub fn index(self) -> usize {
        usize::from(self.0)
    }

    pub fn all() -> impl Iterator<Item = Self> {
        (0..registry().names.len()).map(|index| Self(u16::try_from(index).unwrap_or(u16::MAX)))
    }

    /// Neovim's own definition of a group, used when a theme doesn't define it.
    pub(crate) fn default_definition(name: &str) -> Option<&'static HighlightDefinition> {
        registry().defaults.get(name)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DefaultsFile {
    names: Vec<String>,
    defaults: BTreeMap<String, HighlightDefinition>,
}

struct Registry {
    names: Vec<String>,
    ids: HashMap<String, u16>,
    defaults: BTreeMap<String, HighlightDefinition>,
}

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let file: DefaultsFile =
            toml::from_str(DEFAULTS).expect("bundled highlight defaults must remain valid");
        let mut registry = Registry {
            names: Vec::new(),
            ids: HashMap::new(),
            defaults: BTreeMap::new(),
        };
        let links = file.defaults.values().filter_map(|definition| definition.link.as_deref());
        for name in BUILTIN
            .into_iter()
            .chain(file.names.iter().map(String::as_str))
            .chain(file.defaults.keys().map(String::as_str))
            .chain(links)
        {
            if !registry.ids.contains_key(name) {
                let id = u16::try_from(registry.names.len()).expect("highlight groups fit in u16");
                registry.ids.insert(name.to_owned(), id);
                registry.names.push(name.to_owned());
            }
        }
        registry.defaults = file.defaults;
        registry
    })
}

#[cfg(test)]
mod tests {
    use super::Highlight;

    #[test]
    fn captures_fall_back_to_the_nearest_known_group() {
        assert_eq!(Highlight::for_capture("comment").map(Highlight::name), Some("@comment"));
        assert_eq!(
            Highlight::for_capture("function.method.call").map(Highlight::name),
            Some("@function.method.call")
        );
        assert_eq!(
            Highlight::for_capture("function.method.call.rust").map(Highlight::name),
            Some("@function.method.call")
        );
        assert_eq!(Highlight::for_capture("stumash"), None);
        assert_eq!(
            Highlight::for_semantic_token("derive").map(Highlight::name),
            Some("@lsp.type.derive")
        );
        assert_eq!(Highlight::QUOTE.name(), "@markup.quote");
    }
}
