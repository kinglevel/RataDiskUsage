//! The scanned tree, held in one arena. Ids are handed out by the scanner
//! and are never reused, so a stale id always points at an empty node
//! instead of at somebody else's file.

use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

pub type Id = u32;
pub const NONE: Id = u32::MAX;
/// Extension id for files without a recognisable extension.
pub const NO_EXT: u16 = 0;
/// Longer suffixes are treated as part of the name, not as a type.
const MAX_EXT: usize = 16;

#[derive(Default)]
pub struct Dir {
    pub kids: Vec<Id>,
    /// Bytes per extension for everything below this folder, sorted by id.
    pub types: Vec<(u16, u64)>,
    /// Space taken by the folder entry itself.
    pub own: u64,
    pub sorted: bool,
    pub error: bool,
}

pub struct Node {
    pub name: Box<OsStr>,
    pub parent: Id,
    /// Own size plus everything below.
    pub size: u64,
    /// Number of files below (0 for a file).
    pub count: u32,
    pub ext: u16,
    pub dir: Option<Box<Dir>>,
}

impl Default for Node {
    fn default() -> Self {
        Node { name: OsString::new().into_boxed_os_str(), parent: NONE, size: 0, count: 0, ext: NO_EXT, dir: None }
    }
}

pub struct NewEntry {
    pub name: Box<OsStr>,
    pub size: u64,
    pub is_dir: bool,
}

/// One directory listing from the scanner. Entry `i` gets id `first_id + i`.
pub struct Batch {
    pub dir: Id,
    pub first_id: Id,
    pub entries: Vec<NewEntry>,
    pub error: bool,
}

pub struct Tree {
    pub nodes: Vec<Node>,
    pub root: Id,
    pub root_path: PathBuf,
    /// Folders or entries that could not be read.
    pub errors: u64,
    exts: Vec<Box<str>>,
    ext_ids: HashMap<Box<str>, u16>,
}

fn add(v: &mut Vec<(u16, u64)>, ext: u16, bytes: u64) {
    match v.binary_search_by_key(&ext, |p| p.0) {
        Ok(i) => v[i].1 += bytes,
        Err(i) => v.insert(i, (ext, bytes)),
    }
}

fn sub(v: &mut [(u16, u64)], ext: u16, bytes: u64) {
    if let Ok(i) = v.binary_search_by_key(&ext, |p| p.0) {
        v[i].1 = v[i].1.saturating_sub(bytes);
    }
}

impl Tree {
    pub fn new(root_path: PathBuf, own: u64) -> Self {
        let root = Node { size: own, dir: Some(Box::new(Dir { own, sorted: true, ..Dir::default() })), ..Node::default() };
        Tree { nodes: vec![root], root: 0, root_path, errors: 0, exts: vec!["".into()], ext_ids: HashMap::new() }
    }

    pub fn node(&self, id: Id) -> &Node {
        &self.nodes[id as usize]
    }
    pub fn parent(&self, id: Id) -> Id {
        self.nodes[id as usize].parent
    }
    pub fn size(&self, id: Id) -> u64 {
        self.nodes[id as usize].size
    }
    pub fn is_dir(&self, id: Id) -> bool {
        self.nodes[id as usize].dir.is_some()
    }
    pub fn kids(&self, id: Id) -> &[Id] {
        self.nodes[id as usize].dir.as_ref().map_or(&[], |d| &d.kids)
    }
    pub fn types(&self, id: Id) -> &[(u16, u64)] {
        self.nodes[id as usize].dir.as_ref().map_or(&[], |d| &d.types)
    }
    pub fn ext_name(&self, ext: u16) -> &str {
        self.exts.get(ext as usize).map_or("", |s| s)
    }

    /// Display name: lossy UTF-8 with control characters made visible.
    pub fn name(&self, id: Id) -> String {
        self.nodes[id as usize].name.to_string_lossy().chars().map(|c| if c.is_control() { '?' } else { c }).collect()
    }

    /// The extension of a file, or the type holding most bytes below a folder.
    pub fn dominant(&self, id: Id) -> u16 {
        let n = &self.nodes[id as usize];
        match &n.dir {
            None => n.ext,
            Some(d) => d.types.iter().max_by_key(|t| t.1).filter(|t| t.1 > 0).map_or(NO_EXT, |t| t.0),
        }
    }

    /// Real path of a node. `None` when the node is no longer attached to
    /// the scan root, so a stale id can never resolve to a wrong path.
    pub fn path(&self, id: Id) -> Option<PathBuf> {
        let mut parts = Vec::new();
        let mut a = id;
        while a != self.root {
            if a == NONE || a as usize >= self.nodes.len() {
                return None;
            }
            parts.push(&self.nodes[a as usize].name);
            a = self.nodes[a as usize].parent;
        }
        let mut p = self.root_path.clone();
        for s in parts.iter().rev() {
            if s.is_empty() {
                return None;
            }
            p.push(&***s);
        }
        Some(p)
    }

    /// Path of a folder for display, with a trailing slash and `~` for home.
    pub fn dir_label(&self, id: Id) -> String {
        let root = self.root_path.to_string_lossy().into_owned();
        let mut out = match std::env::var("HOME") {
            Ok(h) if h.len() > 1 && root == h => "~".to_string(),
            Ok(h) if h.len() > 1 && root.starts_with(&format!("{h}/")) => format!("~{}", &root[h.len()..]),
            _ => root,
        };
        let mut parts = Vec::new();
        let mut a = id;
        while a != self.root && a != NONE {
            parts.push(self.name(a));
            a = self.nodes[a as usize].parent;
        }
        for s in parts.iter().rev() {
            if !out.ends_with('/') {
                out.push('/');
            }
            out.push_str(s);
        }
        if !out.ends_with('/') {
            out.push('/');
        }
        out
    }

    fn ext_of(&mut self, name: &OsStr) -> u16 {
        let mut bytes = name.as_encoded_bytes();
        // Version and rotation suffixes are not types: `libc.so.6` is a
        // `.so` and `access.log.1` is a `.log`.
        let (dot, ext) = loop {
            let Some(dot) = bytes.iter().rposition(|&b| b == b'.') else { return NO_EXT };
            let ext = &bytes[dot + 1..];
            if dot > 0 && !ext.is_empty() && ext.len() <= MAX_EXT && ext.iter().all(u8::is_ascii_digit) {
                bytes = &bytes[..dot];
                continue;
            }
            break (dot, ext);
        };
        if dot == 0 || ext.is_empty() || ext.len() > MAX_EXT || !ext.iter().all(u8::is_ascii_alphanumeric) {
            return NO_EXT;
        }
        let key: String = ext.iter().map(|b| b.to_ascii_lowercase() as char).collect();
        if let Some(&id) = self.ext_ids.get(key.as_str()) {
            return id;
        }
        if self.exts.len() >= u16::MAX as usize {
            return NO_EXT;
        }
        let id = self.exts.len() as u16;
        self.exts.push(format!(".{key}").into_boxed_str());
        self.ext_ids.insert(key.into_boxed_str(), id);
        id
    }

    /// Adds one directory listing and carries its totals up to the root.
    pub fn apply(&mut self, b: Batch) {
        let dir = b.dir as usize;
        if dir >= self.nodes.len() || self.nodes[dir].dir.is_none() {
            return;
        }
        if b.error {
            self.errors += 1;
            self.nodes[dir].dir.as_mut().unwrap().error = true;
        }
        if b.entries.is_empty() {
            return;
        }
        let end = b.first_id as usize + b.entries.len();
        if self.nodes.len() < end {
            self.nodes.resize_with(end, Node::default);
        }
        let (mut total, mut files) = (0u64, 0u32);
        let mut types: Vec<(u16, u64)> = Vec::new();
        let mut ids = Vec::with_capacity(b.entries.len());
        for (i, e) in b.entries.into_iter().enumerate() {
            let id = b.first_id + i as Id;
            let ext = if e.is_dir { NO_EXT } else { self.ext_of(&e.name) };
            total += e.size;
            if !e.is_dir {
                files += 1;
                add(&mut types, ext, e.size);
            }
            self.nodes[id as usize] = Node {
                name: e.name,
                parent: b.dir,
                size: e.size,
                count: 0,
                ext,
                dir: e.is_dir.then(|| Box::new(Dir { own: e.size, sorted: true, ..Dir::default() })),
            };
            ids.push(id);
        }
        self.nodes[dir].dir.as_mut().unwrap().kids.extend(ids);
        let mut a = b.dir;
        while a != NONE {
            let n = &mut self.nodes[a as usize];
            n.size += total;
            n.count += files;
            if let Some(d) = n.dir.as_mut() {
                d.sorted = false;
                for &(e, v) in &types {
                    add(&mut d.types, e, v);
                }
            }
            a = n.parent;
        }
    }

    /// Orders a folder's children largest first. Cheap when already sorted.
    pub fn sort_kids(&mut self, id: Id) {
        let Some(d) = self.nodes[id as usize].dir.as_mut() else { return };
        if d.sorted {
            return;
        }
        let mut kids = std::mem::take(&mut d.kids);
        kids.sort_by(|&a, &b| self.nodes[b as usize].size.cmp(&self.nodes[a as usize].size));
        let d = self.nodes[id as usize].dir.as_mut().unwrap();
        d.kids = kids;
        d.sorted = true;
    }

    fn subtract_up(&mut self, from: Id, size: u64, count: u32, types: &[(u16, u64)]) {
        let mut a = from;
        while a != NONE {
            let n = &mut self.nodes[a as usize];
            n.size = n.size.saturating_sub(size);
            n.count = n.count.saturating_sub(count);
            if let Some(d) = n.dir.as_mut() {
                d.sorted = false;
                for &(e, v) in types {
                    sub(&mut d.types, e, v);
                }
            }
            a = n.parent;
        }
    }

    /// Every id below `id`, not including `id` itself.
    pub fn descendants(&self, id: Id) -> HashSet<Id> {
        let mut out = HashSet::new();
        let mut stack: Vec<Id> = self.kids(id).to_vec();
        while let Some(k) = stack.pop() {
            out.insert(k);
            stack.extend_from_slice(self.kids(k));
        }
        out
    }

    fn free_subtree(&mut self, id: Id) {
        let mut stack = vec![id];
        while let Some(k) = stack.pop() {
            let node = std::mem::take(&mut self.nodes[k as usize]);
            if let Some(d) = node.dir {
                stack.extend(d.kids);
            }
        }
    }

    /// Takes a node out of the tree and returns the space it accounted for.
    pub fn remove(&mut self, id: Id) -> u64 {
        let n = &self.nodes[id as usize];
        let (p, size) = (n.parent, n.size);
        if p == NONE {
            return 0;
        }
        let (count, types) = match &n.dir {
            Some(d) => (n.count, d.types.clone()),
            None => (1, vec![(n.ext, n.size)]),
        };
        if let Some(d) = self.nodes[p as usize].dir.as_mut() {
            d.kids.retain(|&k| k != id);
        }
        self.free_subtree(id);
        self.subtract_up(p, size, count, &types);
        size
    }

    /// Empties a folder ahead of a rescan, keeping the folder node itself.
    pub fn clear_children(&mut self, id: Id) {
        let n = &mut self.nodes[id as usize];
        let Some(d) = n.dir.as_mut() else { return };
        let kids = std::mem::take(&mut d.kids);
        let types = std::mem::take(&mut d.types);
        let (gone, count, parent) = (n.size.saturating_sub(d.own), n.count, n.parent);
        n.size = d.own;
        n.count = 0;
        d.sorted = true;
        d.error = false;
        for k in kids {
            self.free_subtree(k);
        }
        self.subtract_up(parent, gone, count, &types);
        if id == self.root {
            self.errors = 0;
        }
    }
}

#[cfg(test)]
pub mod testing {
    use super::*;

    pub fn entry(name: &str, size: u64, is_dir: bool) -> NewEntry {
        NewEntry { name: OsString::from(name).into_boxed_os_str(), size, is_dir }
    }

    /// Builds a tree from `(dir id, first id, entries)` listings.
    pub fn tree(listings: Vec<(Id, Id, Vec<NewEntry>)>) -> Tree {
        let mut t = Tree::new(PathBuf::from("/scan"), 0);
        for (dir, first_id, entries) in listings {
            t.apply(Batch { dir, first_id, entries, error: false });
        }
        t
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    fn sample() -> Tree {
        // /scan/{video.mkv 600, docs/{a.pdf 30, b.PDF 10, notes 5}, src/{main.rs 2}}
        tree(vec![
            (0, 1, vec![entry("video.mkv", 600, false), entry("docs", 4, true), entry("src", 4, true)]),
            (2, 4, vec![entry("a.pdf", 30, false), entry("b.PDF", 10, false), entry("notes", 5, false)]),
            (3, 7, vec![entry("main.rs", 2, false)]),
        ])
    }

    #[test]
    fn totals_roll_up() {
        let t = sample();
        assert_eq!(t.size(0), 600 + 4 + 4 + 45 + 2);
        assert_eq!(t.size(2), 4 + 45);
        assert_eq!(t.node(0).count, 5);
        assert_eq!(t.node(2).count, 3);
    }

    #[test]
    fn extensions_are_case_insensitive_and_strict() {
        let mut t = sample();
        assert_eq!(t.node(4).ext, t.node(5).ext);
        assert_eq!(t.ext_name(t.node(4).ext), ".pdf");
        assert_eq!(t.node(6).ext, NO_EXT);
        let cases = [
            (".bashrc", ""),
            ("backup.tar.gz", ".gz"),
            ("log.2024", ""),
            ("weird.this-is-not", ""),
            ("x.", ""),
            ("libc.so.6", ".so"),
            ("libfoo.so.1.2.3", ".so"),
            ("access.log.1", ".log"),
            (".hidden.7", ""),
            ("Makefile", ""),
            ("model.safetensors", ".safetensors"),
            ("notes.averyveryverylongsuffix", ""),
        ];
        for (name, want) in cases {
            let e = t.ext_of(OsStr::new(name));
            assert_eq!(t.ext_name(e), want, "{name}");
        }
    }

    #[test]
    fn dominant_type_follows_bytes() {
        let t = sample();
        assert_eq!(t.ext_name(t.dominant(0)), ".mkv");
        assert_eq!(t.ext_name(t.dominant(2)), ".pdf");
        assert_eq!(t.ext_name(t.dominant(7)), ".rs");
    }

    #[test]
    fn children_sort_largest_first() {
        let mut t = sample();
        t.sort_kids(0);
        assert_eq!(t.kids(0), &[1, 2, 3]);
        t.sort_kids(2);
        assert_eq!(t.kids(2), &[4, 5, 6]);
    }

    #[test]
    fn paths_resolve_only_while_attached() {
        let mut t = sample();
        assert_eq!(t.path(4), Some(PathBuf::from("/scan/docs/a.pdf")));
        assert_eq!(t.path(0), Some(PathBuf::from("/scan")));
        t.remove(2);
        assert_eq!(t.path(4), None);
        assert_eq!(t.path(2), None);
        assert_eq!(t.path(NONE), None);
    }

    #[test]
    fn remove_subtracts_from_every_ancestor() {
        let mut t = sample();
        let freed = t.remove(2);
        assert_eq!(freed, 49);
        assert_eq!(t.size(0), 600 + 4 + 2);
        assert_eq!(t.node(0).count, 2);
        assert_eq!(t.kids(0), &[1, 3]);
        let pdf = t.ext_ids["pdf"];
        assert_eq!(t.types(0).iter().find(|x| x.0 == pdf).unwrap().1, 0);
    }

    #[test]
    fn clearing_a_folder_keeps_the_folder() {
        let mut t = sample();
        let before = t.descendants(2);
        assert_eq!(before.len(), 3);
        t.clear_children(2);
        assert_eq!(t.size(2), 4);
        assert_eq!(t.size(0), 600 + 4 + 4 + 2);
        assert!(t.kids(2).is_empty());
        assert_eq!(t.node(0).count, 2);
        // The same listing can be applied again, as a rescan would.
        t.apply(Batch { dir: 2, first_id: 20, entries: vec![entry("a.pdf", 30, false)], error: false });
        assert_eq!(t.size(0), 600 + 4 + 4 + 2 + 30);
    }

    #[test]
    fn labels_use_a_trailing_slash() {
        let t = sample();
        assert_eq!(t.dir_label(0), "/scan/");
        assert_eq!(t.dir_label(2), "/scan/docs/");
    }
}
