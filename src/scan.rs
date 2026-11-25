//! Parallel directory scanner. Workers list directories and stream one
//! [`Batch`] per directory to the interface, which owns the tree.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use crate::tree::{Batch, Id, NewEntry};

#[derive(Clone, Copy)]
pub struct Options {
    /// Stay on the filesystem the scan started on.
    pub one_fs: bool,
    /// Count file lengths instead of allocated blocks.
    pub apparent: bool,
    pub threads: usize,
}

struct Shared {
    next_id: AtomicU32,
    /// Hard-linked files already counted, keyed by (device, inode).
    seen: Mutex<HashMap<(u64, u64), Id>>,
    opts: Options,
}

struct Run {
    queue: Mutex<Vec<Job>>,
    cv: Condvar,
    /// Jobs queued or being worked on. The scan is over when it reaches zero.
    pending: AtomicUsize,
}

struct Job {
    id: Id,
    path: PathBuf,
    dev: u64,
}

pub struct Scanner {
    shared: Arc<Shared>,
}

#[cfg(unix)]
fn ids(md: &Metadata) -> (u64, u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (md.dev(), md.ino(), md.nlink())
}
#[cfg(not(unix))]
fn ids(_md: &Metadata) -> (u64, u64, u64) {
    (0, 0, 1)
}

/// Space a single entry takes: allocated blocks, or its length.
pub fn entry_size(md: &Metadata, apparent: bool) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if !apparent {
            return md.blocks() * 512;
        }
    }
    let _ = apparent;
    md.len()
}

/// Kernel filesystems that hold no disk data and are never worth walking.
fn is_kernel_mount(path: &Path) -> bool {
    matches!(path.to_str(), Some("/proc" | "/sys" | "/dev" | "/run"))
}

impl Scanner {
    pub fn new(opts: Options) -> Self {
        Scanner { shared: Arc::new(Shared { next_id: AtomicU32::new(1), seen: Mutex::new(HashMap::new()), opts }) }
    }

    /// Starts scanning `path` as the contents of node `dir`. The receiver
    /// disconnects when the scan is complete.
    pub fn start(&self, dir: Id, path: PathBuf) -> Receiver<Batch> {
        let dev = fs::symlink_metadata(&path).map(|m| ids(&m).0).unwrap_or(0);
        let run = Arc::new(Run { queue: Mutex::new(vec![Job { id: dir, path, dev }]), cv: Condvar::new(), pending: AtomicUsize::new(1) });
        let (tx, rx) = mpsc::channel();
        for _ in 0..self.shared.opts.threads.max(1) {
            let (shared, run, tx) = (self.shared.clone(), run.clone(), tx.clone());
            thread::spawn(move || worker(&shared, &run, tx));
        }
        rx
    }

    /// Forgets hard links counted inside a part of the tree that is being
    /// dropped, so a rescan counts them again.
    pub fn forget(&self, gone: &HashSet<Id>) {
        self.shared.seen.lock().unwrap().retain(|_, id| !gone.contains(id));
    }
}

fn worker(shared: &Shared, run: &Run, tx: Sender<Batch>) {
    loop {
        let job = {
            let mut q = run.queue.lock().unwrap();
            loop {
                if let Some(j) = q.pop() {
                    break Some(j);
                }
                if run.pending.load(Ordering::SeqCst) == 0 {
                    break None;
                }
                q = run.cv.wait(q).unwrap();
            }
        };
        let Some(job) = job else { return };
        let (batch, subdirs) = list(shared, &job);
        // The listing is sent before its subdirectories are queued, so a
        // parent always reaches the tree before its children do.
        let alive = tx.send(batch).is_ok();
        if alive && !subdirs.is_empty() {
            run.pending.fetch_add(subdirs.len(), Ordering::SeqCst);
            run.queue.lock().unwrap().extend(subdirs);
            run.cv.notify_all();
        }
        if !alive {
            run.pending.store(0, Ordering::SeqCst);
            let _guard = run.queue.lock().unwrap();
            run.cv.notify_all();
            return;
        }
        if run.pending.fetch_sub(1, Ordering::SeqCst) == 1 {
            let _guard = run.queue.lock().unwrap();
            run.cv.notify_all();
        }
    }
}

fn list(shared: &Shared, job: &Job) -> (Batch, Vec<Job>) {
    let failed = |error| (Batch { dir: job.id, first_id: 0, entries: Vec::new(), error }, Vec::new());
    let Ok(rd) = fs::read_dir(&job.path) else { return failed(true) };
    let mut error = false;
    let mut found: Vec<(NewEntry, (u64, u64, u64))> = Vec::new();
    for de in rd {
        // DirEntry::metadata does not follow symlinks, so links are counted
        // as the small entries they are and never walked.
        let (de, md) = match de.and_then(|d| d.metadata().map(|m| (d, m))) {
            Ok(v) => v,
            Err(_) => {
                error = true;
                continue;
            }
        };
        let entry = NewEntry { name: de.file_name().into_boxed_os_str(), size: entry_size(&md, shared.opts.apparent), is_dir: md.is_dir() };
        found.push((entry, ids(&md)));
    }
    if found.is_empty() {
        return failed(error);
    }
    let first_id = shared.next_id.fetch_add(found.len() as u32, Ordering::SeqCst);
    let mut entries = Vec::with_capacity(found.len());
    let mut subdirs = Vec::new();
    for (i, (mut entry, (dev, ino, nlink))) in found.into_iter().enumerate() {
        let id = first_id + i as Id;
        if entry.is_dir {
            let path = job.path.join(&*entry.name);
            let other_fs = dev != job.dev;
            if !(other_fs && (shared.opts.one_fs || is_kernel_mount(&path))) {
                subdirs.push(Job { id, path, dev });
            }
        } else if nlink > 1 {
            // The first link seen carries the bytes, later ones count as zero.
            match shared.seen.lock().unwrap().entry((dev, ino)) {
                Entry::Occupied(_) => entry.size = 0,
                Entry::Vacant(slot) => {
                    slot.insert(id);
                }
            }
        }
        entries.push(entry);
    }
    (Batch { dir: job.id, first_id, entries, error }, subdirs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::Tree;
    use std::fs::File;
    use std::io::Write;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("radu-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, bytes: usize) {
        File::create(path).unwrap().write_all(&vec![7u8; bytes]).unwrap();
    }

    fn scan(root: &Path, apparent: bool) -> Tree {
        let own = entry_size(&fs::symlink_metadata(root).unwrap(), apparent);
        let mut tree = Tree::new(root.to_path_buf(), own);
        let scanner = Scanner::new(Options { one_fs: false, apparent, threads: 4 });
        for batch in scanner.start(tree.root, root.to_path_buf()) {
            tree.apply(batch);
        }
        tree
    }

    fn find(tree: &Tree, dir: Id, name: &str) -> Id {
        *tree.kids(dir).iter().find(|&&k| tree.name(k) == name).unwrap_or_else(|| panic!("{name} not found"))
    }

    #[test]
    fn scans_nested_folders_and_counts_lengths() {
        let root = scratch("nested");
        fs::create_dir_all(root.join("a/b")).unwrap();
        write(&root.join("top.bin"), 3000);
        write(&root.join("a/one.txt"), 100);
        write(&root.join("a/b/two.txt"), 250);
        let tree = scan(&root, true);
        let a = find(&tree, tree.root, "a");
        let b = find(&tree, a, "b");
        assert_eq!(tree.node(tree.root).count, 3);
        assert_eq!(tree.size(find(&tree, tree.root, "top.bin")), 3000);
        assert_eq!(tree.size(find(&tree, b, "two.txt")), 250);
        assert_eq!(tree.size(a), tree.node(a).dir.as_ref().unwrap().own + 100 + tree.size(b));
        assert_eq!(tree.path(find(&tree, b, "two.txt")), Some(root.join("a/b/two.txt")));
        assert_eq!(tree.errors, 0);
        fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn hard_links_count_once_and_symlinks_are_not_followed() {
        let root = scratch("links");
        fs::create_dir_all(root.join("real")).unwrap();
        write(&root.join("real/data.bin"), 5000);
        fs::hard_link(root.join("real/data.bin"), root.join("twin.bin")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("shortcut")).unwrap();
        let tree = scan(&root, true);
        let real = find(&tree, tree.root, "real");
        let sizes = [tree.size(find(&tree, real, "data.bin")), tree.size(find(&tree, tree.root, "twin.bin"))];
        assert_eq!(sizes.iter().sum::<u64>(), 5000, "one of the two links carries the bytes");
        let link = find(&tree, tree.root, "shortcut");
        assert!(!tree.is_dir(link));
        assert_eq!(tree.node(tree.root).count, 3);
        fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_folders_are_reported_not_fatal() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch("denied");
        fs::create_dir_all(root.join("locked")).unwrap();
        write(&root.join("locked/secret"), 10);
        write(&root.join("open.txt"), 10);
        fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o000)).unwrap();
        let readable_anyway = fs::read_dir(root.join("locked")).is_ok(); // true when running as root
        let tree = scan(&root, true);
        fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o755)).unwrap();
        if !readable_anyway {
            assert_eq!(tree.errors, 1);
            assert!(tree.node(find(&tree, tree.root, "locked")).dir.as_ref().unwrap().error);
        }
        assert_eq!(tree.size(find(&tree, tree.root, "open.txt")), 10);
        fs::remove_dir_all(&root).unwrap();
    }

    /// The whole point of the tool: the total must be what `du` reports.
    #[cfg(unix)]
    #[test]
    fn disk_usage_matches_du() {
        let root = scratch("du");
        fs::create_dir_all(root.join("a/b/c")).unwrap();
        write(&root.join("big.bin"), 300_000);
        write(&root.join("a/one"), 5_000);
        write(&root.join("a/b/two"), 70_000);
        write(&root.join("a/b/c/empty"), 0);
        fs::hard_link(root.join("big.bin"), root.join("a/big-again.bin")).unwrap();
        std::os::unix::fs::symlink("big.bin", root.join("link")).unwrap();
        let tree = scan(&root, false);
        // `-B1` is GNU du. Where du is missing or different, skip the comparison.
        if let Ok(out) = std::process::Command::new("du").args(["-s", "-B1"]).arg(&root).output() {
            if out.status.success() {
                let du: u64 = String::from_utf8_lossy(&out.stdout).split_whitespace().next().unwrap().parse().unwrap();
                assert_eq!(tree.size(tree.root), du);
            }
        }
        assert_eq!(tree.node(tree.root).count, 6);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn forgetting_ids_lets_links_count_again() {
        let s = Scanner::new(Options { one_fs: false, apparent: true, threads: 1 });
        s.shared.seen.lock().unwrap().extend([((1, 10), 5), ((1, 11), 6)]);
        s.forget(&HashSet::from([5]));
        let seen = s.shared.seen.lock().unwrap();
        assert!(!seen.contains_key(&(1, 10)) && seen.contains_key(&(1, 11)));
    }
}
