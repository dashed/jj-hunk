//! `list --fields`: which parts of a listing are actually sent.
//!
//! `list --format json` emits every hunk's full `removed` and `added` text.
//! On a real diff that text *is* the response -- the ids, ranges and paths
//! around it are a rounding error -- and the documented agent loop is
//! list -> preview -> act by id, which reads none of it. The caller pays for
//! the diff twice: once here, and again when it opens the file. A mask lets it
//! ask for the skeleton and leave the bytes behind.
//!
//! # The mask is an allow-list, and it cannot forge an absence
//!
//! Naming a field keeps it; naming nothing keeps nothing. That direction is
//! the safe one for content -- a caller that under-asks gets less than it
//! wanted and can ask again -- but it is the *unsafe* one for the file-level
//! flags, because every one of them is already absent in the ordinary case.
//! `truncated`, `binary`, `symlink`, `mode` and `rename` are serialised only
//! when they are true, so "no `truncated` key" already means "not truncated",
//! and a mask able to suppress the key would make the two indistinguishable.
//!
//! Two of the five are not merely informative:
//!
//! - `truncated` says the hunks listed for a file describe only its opening
//!   slice. A caller that cannot see it believes it saw the whole diff.
//! - `rename.from` is the pre-image path. The wrapper verbs re-derive it (see
//!   `fill_rename_sources`), but the documented raw `jj --tool=jj-hunk` path
//!   does not: there, a spec that omits `from` drops the renamed file from the
//!   commit entirely. That is data loss caused by a field the caller was never
//!   told it needed.
//!
//! The other three -- `binary`, `symlink`, `mode` -- are why a file that
//! plainly changed carries an empty `hunks` array. Without them that entry
//! reads as "nothing to do here", which is the reading that leaves a binary
//! behind at exit 0.
//!
//! So all five are emitted whether or not they are named. They cost nothing
//! when nothing happened, which is the overwhelming majority of entries, and
//! the saving this feature exists for is in `removed`/`added`/`context`
//! anyway. Naming one is accepted and redundant rather than an error: refusing
//! a name that *is* a key in the output would be the more surprising rule.
//!
//! # Unknown names are refused
//!
//! An allow-list makes a typo indistinguishable from a deliberate omission:
//! `--fields "paht,id"` would answer with entries that have no `path`, and a
//! caller would conclude the diff holds files with no path. So a name that is
//! not a field is [`errors::INVALID_FIELDS`], and `details.valid_fields`
//! carries the whole list so the caller can correct itself without a human.
//!
//! # Order is part of the shape
//!
//! Masking happens over [`Node`], which remembers the order its keys arrived
//! in, and the listing is handed to it as JSON *text* rather than as a
//! `serde_json::Value` -- whose object is a `BTreeMap` and would alphabetise
//! every level: `added` before `id` before `path`, `lines` before `start`,
//! `post` before `pre`. Parsing the text back is what recovers the declared
//! order, at every depth and without this module knowing the shape of anything
//! nested. `masking_everything_reproduces_the_unmasked_bytes` holds it there.

use crate::errors::{self, CodedError};
use anyhow::Result;
use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fmt;

/// Every key a file entry can carry, in the order `FileEntry` declares them,
/// paired with whether a mask may drop it.
///
/// One table rather than three lists, so the names `--fields` accepts and the
/// ones it refuses to drop cannot drift apart.
const FILE_FIELDS: &[(&str, Always)] = &[
    ("path", Always::No),
    ("status", Always::No),
    ("rename", Always::Yes),
    ("hunks", Always::No),
    ("binary", Always::Yes),
    ("mode", Always::Yes),
    ("symlink", Always::Yes),
    ("truncated", Always::Yes),
];

/// Every key a hunk can carry, in the order `Hunk` declares them. The tail
/// from `enclosing_function` on is `SemanticInfo`, which `Hunk` flattens --
/// there is no `semantic` key in the output, only its contents.
///
/// The list is the same in both feature modes. `SemanticInfo` is not behind
/// `#[cfg(feature = "semantic")]`; without the feature the analyser simply
/// never runs, every value stays at its default, and `skip_serializing_if`
/// drops them all. So the accepted names do not depend on how the binary was
/// built, and a caller never has to ask which build it is talking to.
const HUNK_FIELDS: &[&str] = &[
    "index",
    "id",
    "short_id",
    "type",
    "removed",
    "added",
    "before",
    "after",
    "context",
    "enclosing_function",
    "enclosing_scope",
    "annotations",
    "is_doc_comment",
    "is_import",
    "is_toplevel",
    "nesting_depth",
    "is_analyzed",
];

/// Where `SemanticInfo` starts inside [`HUNK_FIELDS`]. `hunks.semantic` names
/// the tail from here on: the struct has that name in Rust and in the docs, so
/// asking for it by that name has to work even though the wire has no such key.
const SEMANTIC_START: usize = 9;

/// The key holding a file's hunks, and the prefix a hunk field is spelled with.
const HUNKS: &str = "hunks";

/// The alias for all of `SemanticInfo` at once.
const SEMANTIC: &str = "semantic";

/// The two container keys a mask recurses through. Everything else in the
/// envelope -- a group's `name` -- is structure, not payload: masking it away
/// would leave a group's files in an unlabelled bag.
const FILES: &str = "files";
const GROUPS: &str = "groups";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Always {
    Yes,
    No,
}

/// Which fields of a listing to emit.
#[derive(Debug, Clone, Default)]
pub struct FieldMask {
    file: BTreeSet<&'static str>,
    hunk: BTreeSet<&'static str>,
}

impl FieldMask {
    /// Parse `--fields` values into a mask, or refuse.
    ///
    /// Values arrive already split on commas by clap, and `--fields` is
    /// repeatable, so `--fields path --fields hunks.id` and
    /// `--fields path,hunks.id` are the same mask. Blank entries are dropped
    /// rather than refused -- `path,,id` is a stutter, not a misunderstanding --
    /// but a mask blank all through is refused, because a listing with no
    /// fields in it is nobody's intent.
    pub fn parse(values: &[String]) -> Result<Self> {
        let mut mask = Self::default();
        let mut unknown: Vec<String> = Vec::new();

        for value in values {
            let name = value.trim();
            if name.is_empty() {
                continue;
            }
            if !mask.add(name) {
                unknown.push(name.to_string());
            }
        }

        if !unknown.is_empty() {
            let listed = unknown
                .iter()
                .map(|name| format!("'{name}'"))
                .collect::<Vec<_>>()
                .join(", ");
            let noun = if unknown.len() == 1 { "field" } else { "fields" };
            return Err(refusal(
                format!("--fields names no such output {noun}: {listed}"),
                unknown,
            ));
        }

        if mask.is_empty() {
            return Err(refusal(
                "--fields requires at least one field name".to_string(),
                Vec::new(),
            ));
        }

        Ok(mask)
    }

    /// Add one name, reporting whether it named anything.
    ///
    /// A hunk field may be written `hunks.id` or bare `id`. The dotted form is
    /// canonical -- it is what `valid_fields` hands back, and it says where the
    /// field lives -- and the bare form is accepted because it is the obvious
    /// thing to type and there is nothing for it to collide with. That last
    /// part is a fact about today's shape, not a wish:
    /// `the_two_levels_share_no_field_name` fails the moment it stops holding,
    /// so a future collision becomes a decision someone has to make rather than
    /// a silent pick.
    fn add(&mut self, name: &str) -> bool {
        if name == HUNKS {
            self.hunk.extend(HUNK_FIELDS);
            return true;
        }
        if let Some(field) = name
            .strip_prefix(HUNKS)
            .and_then(|rest| rest.strip_prefix('.'))
        {
            return self.add_hunk_field(field);
        }

        if let Some((key, _)) = FILE_FIELDS.iter().find(|(key, _)| *key == name) {
            self.file.insert(key);
            return true;
        }

        self.add_hunk_field(name)
    }

    fn add_hunk_field(&mut self, name: &str) -> bool {
        if name == SEMANTIC {
            self.hunk.extend(&HUNK_FIELDS[SEMANTIC_START..]);
            return true;
        }
        match HUNK_FIELDS.iter().find(|key| **key == name) {
            Some(key) => {
                self.hunk.insert(key);
                true
            }
            None => false,
        }
    }

    fn is_empty(&self) -> bool {
        self.file.is_empty() && self.hunk.is_empty()
    }

    /// Serialise a listing and hand back only the parts that were asked for.
    ///
    /// Goes through JSON text rather than the structs so that `SemanticInfo`,
    /// which `Hunk` flattens, is a set of ordinary keys here exactly as it is
    /// to a caller -- there is no `semantic` field on the wire to mask -- and
    /// so this module needs to know nothing about the types in `commands`.
    pub fn apply<T: Serialize>(&self, output: &T) -> Result<Node> {
        let text = serde_json::to_string(output)?;
        let node: Node = serde_json::from_str(&text)?;
        Ok(self.mask_envelope(node))
    }

    fn mask_envelope(&self, node: Node) -> Node {
        node.map_object(|key, value| match key {
            FILES => self.mask_files(value),
            GROUPS => value.map_array(|group| {
                group.map_object(|key, value| match key {
                    FILES => self.mask_files(value),
                    _ => value,
                })
            }),
            _ => value,
        })
    }

    fn mask_files(&self, node: Node) -> Node {
        node.map_array(|file| self.mask_file(file))
    }

    fn mask_file(&self, node: Node) -> Node {
        let Node::Object(entries) = node else {
            return node;
        };

        let mut kept = Vec::with_capacity(entries.len());
        for (key, value) in entries {
            if key == HUNKS {
                // Not governed by its own name: `hunks` is where hunk fields
                // live, so it is present exactly when one of them was asked
                // for. Emitting `"hunks": []` when none was would be a lie --
                // an empty array means "this file has no hunks".
                if !self.hunk.is_empty() {
                    kept.push((key, value.map_array(|hunk| self.mask_hunk(hunk))));
                }
                continue;
            }
            if keep_file_key(&key, &self.file) {
                kept.push((key, value));
            }
        }
        Node::Object(kept)
    }

    fn mask_hunk(&self, node: Node) -> Node {
        let Node::Object(entries) = node else {
            return node;
        };
        Node::Object(
            entries
                .into_iter()
                .filter(|(key, _)| keep_hunk_key(key, &self.hunk))
                .collect(),
        )
    }
}

/// A key survives if it was asked for, if it is one of the flags a mask cannot
/// drop, or if this module has never heard of it.
///
/// That last clause is the important one. A field added to `FileEntry` or
/// `Hunk` without being registered here would otherwise vanish from every
/// masked listing, and vanish *quietly*: the caller asked for a subset, so a
/// smaller object is exactly what it expects.
/// `the_registry_covers_every_serialised_key` fails in CI when that happens,
/// and until someone acts on it the failure mode is a field that cannot be
/// masked rather than one that cannot be seen.
fn keep_file_key(key: &str, asked_for: &BTreeSet<&'static str>) -> bool {
    match FILE_FIELDS.iter().find(|(known, _)| *known == key) {
        Some((_, Always::Yes)) => true,
        Some((known, Always::No)) => asked_for.contains(known),
        None => true,
    }
}

fn keep_hunk_key(key: &str, asked_for: &BTreeSet<&'static str>) -> bool {
    !HUNK_FIELDS.contains(&key) || asked_for.contains(key)
}

/// Every name `--fields` accepts, in the order the fields appear in a listing.
///
/// Alphabetical would be easier to scan and worse to act on: a caller reading
/// this back is choosing a subset of a shape, and the shape's own order is
/// what tells it which names belong to a file and which to a hunk.
fn valid_names() -> Vec<String> {
    let mut names: Vec<String> = FILE_FIELDS
        .iter()
        .map(|(key, _)| (*key).to_string())
        .collect();
    names.extend(HUNK_FIELDS.iter().map(|key| format!("{HUNKS}.{key}")));
    names.push(format!("{HUNKS}.{SEMANTIC}"));
    names
}

/// The file-level flags a mask cannot drop.
fn always_included() -> Vec<String> {
    FILE_FIELDS
        .iter()
        .filter(|(_, always)| *always == Always::Yes)
        .map(|(key, _)| (*key).to_string())
        .collect()
}

/// The valid names go in the message as well as in `details`, because human
/// mode has nowhere else to put them and this is the one error a caller is
/// expected to fix and immediately retry.
fn refusal(message: String, rejected: Vec<String>) -> anyhow::Error {
    let valid = valid_names();
    CodedError::new(
        errors::INVALID_FIELDS,
        format!("{message}\nvalid fields: {}", valid.join(", ")),
    )
    .with("fields", rejected)
    .with("valid_fields", valid)
    .with("always_included", always_included())
    .into()
}

/// A JSON value whose objects remember the order their keys arrived in.
///
/// `serde_json::Value` cannot: its object is a `BTreeMap` unless the whole
/// crate opts into `preserve_order`, which would also silently reorder
/// `details` in every structured error.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Leaf(Value),
    Array(Vec<Node>),
    Object(Vec<(String, Node)>),
}

impl Node {
    fn map_object(self, mut each: impl FnMut(&str, Node) -> Node) -> Node {
        match self {
            Node::Object(entries) => Node::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| {
                        let mapped = each(&key, value);
                        (key, mapped)
                    })
                    .collect(),
            ),
            other => other,
        }
    }

    fn map_array(self, mut each: impl FnMut(Node) -> Node) -> Node {
        match self {
            Node::Array(items) => Node::Array(items.into_iter().map(&mut each).collect()),
            other => other,
        }
    }
}

impl Serialize for Node {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Node::Leaf(value) => value.serialize(serializer),
            Node::Array(items) => {
                let mut seq = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(item)?;
                }
                seq.end()
            }
            Node::Object(entries) => {
                let mut map = serializer.serialize_map(Some(entries.len()))?;
                for (key, value) in entries {
                    map.serialize_entry(key, value)?;
                }
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for Node {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(NodeVisitor)
    }
}

struct NodeVisitor;

impl<'de> Visitor<'de> for NodeVisitor {
    type Value = Node;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Node, A::Error> {
        let mut entries = Vec::with_capacity(map.size_hint().unwrap_or(0));
        while let Some(entry) = map.next_entry::<String, Node>()? {
            entries.push(entry);
        }
        Ok(Node::Object(entries))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Node, A::Error> {
        let mut items = Vec::with_capacity(seq.size_hint().unwrap_or(0));
        while let Some(item) = seq.next_element::<Node>()? {
            items.push(item);
        }
        Ok(Node::Array(items))
    }

    fn visit_bool<E>(self, value: bool) -> Result<Node, E> {
        Ok(Node::Leaf(Value::from(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Node, E> {
        Ok(Node::Leaf(Value::from(value)))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Node, E> {
        Ok(Node::Leaf(Value::from(value)))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Node, E> {
        Ok(Node::Leaf(Value::from(value)))
    }

    fn visit_str<E>(self, value: &str) -> Result<Node, E> {
        Ok(Node::Leaf(Value::from(value)))
    }

    fn visit_unit<E>(self) -> Result<Node, E> {
        Ok(Node::Leaf(Value::Null))
    }

    fn visit_none<E>(self) -> Result<Node, E> {
        Ok(Node::Leaf(Value::Null))
    }
}

/// The keys a file entry may carry, for the test that holds this registry to
/// what the serialiser actually emits.
#[cfg(test)]
pub(crate) fn registered_file_keys() -> Vec<&'static str> {
    FILE_FIELDS.iter().map(|(key, _)| *key).collect()
}

/// The keys a hunk may carry. See [`registered_file_keys`].
#[cfg(test)]
pub(crate) fn registered_hunk_keys() -> Vec<&'static str> {
    HUNK_FIELDS.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(spec: &str) -> Vec<String> {
        spec.split(',').map(str::to_string).collect()
    }

    fn mask(spec: &str) -> FieldMask {
        FieldMask::parse(&names(spec)).expect("mask should parse")
    }

    fn rejection(spec: &str) -> Value {
        let err = FieldMask::parse(&names(spec)).expect_err("mask should be refused");
        let coded = err
            .chain()
            .find_map(|link| link.downcast_ref::<CodedError>())
            .expect("the refusal must carry a code");
        Value::Object(coded.details().clone())
    }

    /// The bare spelling only works because no name means one thing at the
    /// file level and another at the hunk level. Nothing enforces that but
    /// this: add a `path` to `Hunk` or an `id` to `FileEntry` and
    /// `--fields path` becomes a coin toss.
    #[test]
    fn the_two_levels_share_no_field_name() {
        for (file_key, _) in FILE_FIELDS {
            assert!(
                !HUNK_FIELDS.contains(file_key),
                "'{file_key}' is both a file field and a hunk field, so the \
                 bare spelling of it is ambiguous"
            );
        }
        // `hunks` is the container and `semantic` an alias for a group of hunk
        // fields; neither may also be an ordinary field name.
        assert!(!HUNK_FIELDS.contains(&HUNKS));
        assert!(!HUNK_FIELDS.contains(&SEMANTIC));
        assert!(!FILE_FIELDS.iter().any(|(key, _)| *key == SEMANTIC));
    }

    /// `SEMANTIC_START` is an index into a list someone will reorder. If it
    /// slips, `hunks.semantic` starts naming `context` -- or stops naming
    /// `enclosing_function` -- and nothing else would notice.
    #[test]
    fn the_semantic_group_is_exactly_the_flattened_struct() {
        assert_eq!(
            &HUNK_FIELDS[SEMANTIC_START..],
            &[
                "enclosing_function",
                "enclosing_scope",
                "annotations",
                "is_doc_comment",
                "is_import",
                "is_toplevel",
                "nesting_depth",
                "is_analyzed",
            ]
        );
        assert_eq!(mask("hunks.semantic").hunk.len(), 8);
    }

    /// The dotted spelling is canonical and the bare one is shorthand, so the
    /// two have to produce the same mask -- otherwise the shorthand is a
    /// second, subtly different feature.
    #[test]
    fn dotted_and_bare_hunk_names_mean_the_same_thing() {
        assert_eq!(mask("hunks.id").hunk, mask("id").hunk);
        assert_eq!(mask("path,hunks.type").file, mask("path,type").file);
        assert_eq!(mask("path,hunks.type").hunk, mask("path,type").hunk);
    }

    /// `hunks` on its own is the whole hunk, which is the difference between
    /// "drop the diff text" and "enumerate seventeen names".
    #[test]
    fn hunks_alone_names_every_hunk_field() {
        assert_eq!(mask(HUNKS).hunk.len(), HUNK_FIELDS.len());
        assert!(mask(HUNKS).file.is_empty());
    }

    /// A file field is never reachable through the `hunks.` prefix. Without
    /// this, `hunks.path` would quietly mean `path` and produce a file key
    /// from a name that says it is a hunk key.
    #[test]
    fn the_hunks_prefix_does_not_reach_file_fields() {
        assert_eq!(rejection("hunks.path")["fields"][0], "hunks.path");
        assert_eq!(rejection("files.path")["fields"][0], "files.path");
    }

    /// A name that merely starts with `hunks` is not a prefixed name. Cheap to
    /// get wrong with `strip_prefix`, and the failure would be a plausible
    /// name accepted as something else entirely.
    #[test]
    fn a_name_beginning_with_hunks_is_not_a_prefix() {
        assert_eq!(rejection("hunkset")["fields"][0], "hunkset");
    }

    /// The typo that motivates refusing unknown names at all. `details` has to
    /// carry enough for a caller to fix itself: what it said, and what it
    /// could have said.
    #[test]
    fn a_typo_is_refused_with_the_list_to_correct_it_from() {
        let details = rejection("paht,id");
        assert_eq!(details["fields"], serde_json::json!(["paht"]));

        let valid = details["valid_fields"].as_array().unwrap();
        assert!(valid.iter().any(|name| name == "path"), "{valid:?}");
        assert!(valid.iter().any(|name| name == "hunks.id"), "{valid:?}");
        // The flags a mask cannot drop are named, so a caller is not left
        // wondering why `rename` came back when it asked only for `path`.
        assert_eq!(
            details["always_included"],
            serde_json::json!(["rename", "binary", "mode", "symlink", "truncated"])
        );
    }

    /// Every bad name at once, not just the first: an agent that fixes one
    /// typo per round trip pays for every round trip.
    #[test]
    fn every_unknown_name_is_reported_together() {
        assert_eq!(
            rejection("paht,id,tpye")["fields"],
            serde_json::json!(["paht", "tpye"])
        );
    }

    /// A blank entry is a stutter in a joined list, not a request for nothing.
    /// A mask blank all through is neither, and answering it with empty
    /// objects would look like a diff of files that have nothing in them.
    #[test]
    fn blank_entries_are_forgiven_but_a_blank_mask_is_not() {
        assert_eq!(mask("path,,hunks.id").file.len(), 1);
        assert_eq!(rejection(",")["fields"], serde_json::json!([]));
        assert_eq!(rejection("")["fields"], serde_json::json!([]));
    }

    /// Ordering is not recovered by luck. Text -> `Node` has to preserve the
    /// order at every depth, including inside the nested objects a mask never
    /// looks at -- `{"start","lines"}` alphabetises to `{"lines","start"}`,
    /// and `serde_json::Value` would have done exactly that.
    #[test]
    fn parsing_preserves_key_order_at_every_depth() {
        let text = r#"{"z":1,"a":{"start":2,"lines":3},"m":[{"pre":"x","post":"y"}]}"#;
        let node: Node = serde_json::from_str(text).expect("valid json");
        assert_eq!(serde_json::to_string(&node).expect("re-serialises"), text);
    }
}
