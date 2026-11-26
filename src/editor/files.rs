use std::{
    collections::{HashMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
};

use notify::{RecursiveMode, Watcher};

pub struct Entry {
    pub path: PathBuf,
    pub dir: bool,
}

pub struct Files {
    pub root: PathBuf,
    pub display_root: String,
    expanded: HashMap<PathBuf, Vec<Entry>>,
    pub rows: Vec<(PathBuf, bool, usize)>,
    pub scroll: usize,
    watcher: notify::RecommendedWatcher,
    watching: HashSet<PathBuf>,
}

impl Files {
    pub fn new(root: PathBuf, watcher: notify::RecommendedWatcher) -> io::Result<Self> {
        let mut files = Self {
            display_root: display_directory(&root),
            root: root.clone(),
            expanded: HashMap::new(),
            rows: Vec::new(),
            scroll: 0,
            watcher,
            watching: HashSet::new(),
        };
        files.watch_directory(&root)?;
        files.read_dir(&root)?;
        files.flatten();
        Ok(files)
    }

    fn watch_directory(&mut self, path: &Path) -> io::Result<()> {
        if !self.watching.contains(path) {
            self.watcher
                .watch(path, RecursiveMode::NonRecursive)
                .map_err(io::Error::other)?;
            self.watching.insert(path.to_owned());
        }
        Ok(())
    }

    fn list_dir(path: &Path) -> io::Result<Vec<Entry>> {
        let mut entries = Vec::new();
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            if entry.file_name() == ".git" {
                continue;
            }
            let kind = entry.file_type()?;
            if kind.is_dir() || kind.is_file() || kind.is_symlink() {
                entries.push(Entry {
                    path: entry.path(),
                    dir: kind.is_dir(),
                });
            }
        }
        entries.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.path.cmp(&b.path)));
        Ok(entries)
    }

    fn read_dir(&mut self, path: &Path) -> io::Result<()> {
        self.expanded.insert(path.to_owned(), Self::list_dir(path)?);
        Ok(())
    }

    pub fn enter(&mut self, path: &Path) -> io::Result<()> {
        let root = path.canonicalize()?;
        let previously_watched = self.watching.contains(&root);
        self.watch_directory(&root)?;
        let listing = Self::list_dir(&root).and_then(|entries| {
            std::env::set_current_dir(&root)?;
            Ok(entries)
        });
        let entries = match listing {
            Ok(entries) => entries,
            Err(err) => {
                if !previously_watched {
                    let _ = self.watcher.unwatch(&root);
                    self.watching.remove(&root);
                }
                return Err(err);
            }
        };
        // The editor is a separate process. Its absolute document paths remain valid.
        self.expanded.clear();
        self.expanded.insert(root.clone(), entries);
        self.display_root = display_directory(&root);
        self.root = root;
        self.scroll = 0;
        self.flatten();
        Ok(())
    }

    pub fn entry_changed(&mut self, path: &Path) -> io::Result<()> {
        self.expanded.retain(|p, _| !p.starts_with(path));
        if let Some(parent) = path.parent() {
            self.read_dir(parent)?;
        }
        self.flatten();
        Ok(())
    }

    fn flatten(&mut self) {
        fn visit(
            map: &HashMap<PathBuf, Vec<Entry>>,
            dir: &Path,
            depth: usize,
            rows: &mut Vec<(PathBuf, bool, usize)>,
        ) {
            if let Some(entries) = map.get(dir) {
                for e in entries {
                    rows.push((e.path.clone(), e.dir, depth));
                    if e.dir {
                        visit(map, &e.path, depth + 1, rows);
                    }
                }
            }
        }
        self.rows.clear();
        visit(&self.expanded, &self.root, 0, &mut self.rows);
        self.scroll = self.scroll.min(self.rows.len().saturating_sub(1));
    }

    pub fn toggle(&mut self, path: &Path) -> io::Result<()> {
        if self.expanded.contains_key(path) {
            self.expanded.retain(|p, _| !p.starts_with(path));
        } else {
            self.read_dir(path)?;
        }
        self.flatten();
        Ok(())
    }

    pub fn is_open(&self, path: &Path) -> bool {
        self.expanded.contains_key(path)
    }

    pub fn refresh(&mut self, parents: &HashSet<PathBuf>, all: bool) -> io::Result<()> {
        let dirs: Vec<_> = self
            .expanded
            .keys()
            .filter(|p| all || parents.contains(*p))
            .cloned()
            .collect();
        if dirs.is_empty() {
            return Ok(());
        }
        for dir in dirs {
            match self.read_dir(&dir) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    self.expanded.remove(&dir);
                }
                other => other?,
            }
        }
        self.flatten();
        Ok(())
    }

    pub fn watch(&mut self, documents: &[super::FileTab]) -> io::Result<()> {
        let mut desired: HashSet<_> = self.expanded.keys().cloned().collect();
        desired.extend(
            documents
                .iter()
                .filter_map(|d| d.path().parent().map(Path::to_owned)),
        );
        for path in self.watching.difference(&desired) {
            let _ = self.watcher.unwatch(path);
        }
        self.watching.retain(|p| desired.contains(p));
        for path in desired {
            if !self.watching.contains(&path) && path.is_dir() {
                self.watch_directory(&path)?;
            }
        }
        Ok(())
    }
}

fn display_directory(path: &Path) -> String {
    if let Some(home) = std::env::home_dir() {
        if let Ok(rest) = path.strip_prefix(home) {
            return if rest.as_os_str().is_empty() {
                "~".into()
            } else {
                Path::new("~").join(rest).display().to_string()
            };
        }
    }
    path.display().to_string()
}
