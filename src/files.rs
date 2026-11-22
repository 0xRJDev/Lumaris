// The images of a folder, in natural order ("img2" before "img10").
use std::cmp::Ordering;
use std::path::{Path, PathBuf};

const EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "apng", "gif", "webp", "bmp", "dib", "tif", "tiff", "qoi", "ico", "tga", "pnm", "pbm", "pgm",
    "ppm", "pam", "svg", "svgz", "avif", "heic", "heif", "hif", "jxl", "exr", "hdr", "dds", "ff", "psd", "psb", "kra", "ora", "jp2", "j2k",
    "j2c", "jpf", "jpx", "jpc", "xcf", "pdf",
];

pub fn is_supported(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.iter().chain(crate::decode::RAW_EXTENSIONS).any(|x| x.eq_ignore_ascii_case(e)))
}

/// A hidden file: its name starts with a dot (".image.png").
pub fn is_hidden(path: &Path) -> bool {
    path.file_name().is_some_and(|n| n.as_encoded_bytes().first() == Some(&b'.'))
}

pub struct FileList {
    pub files: Vec<PathBuf>,
    pub index: usize,
}

impl FileList {
    /// `arg` is an image (its folder is listed), a folder (starts at its first image) or a
    /// comic archive (.cbz/.zip: its images, as if it were a folder).
    /// Hidden images are left out when the IgnoreHidden setting is on, unless opened explicitly.
    pub fn new(arg: &Path) -> Option<FileList> {
        Self::list(arg, crate::settings::current().bool("IgnoreHidden"))
    }

    fn list(arg: &Path, skip_hidden: bool) -> Option<FileList> {
        let arg = std::fs::canonicalize(arg).ok()?;
        if crate::archive::is_archive(&arg) {
            let mut files: Vec<PathBuf> = crate::archive::images(&arg)?
                .into_iter()
                .filter(|name| !skip_hidden || !name.split('/').any(|part| part.starts_with('.')))
                .map(|name| arg.join(name))
                .collect();
            sort_by_path(&mut files);
            return (!files.is_empty()).then_some(FileList { files, index: 0 });
        }
        let (dir, start) = if arg.is_dir() { (arg.clone(), None) } else { (arg.parent()?.to_path_buf(), Some(arg)) };
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_file() || t.is_symlink()))
                    .map(|e| e.path())
                    .filter(|p| is_supported(p) && (!skip_hidden || !is_hidden(p)))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(s) = &start
            && !files.contains(s)
        {
            files.push(s.clone()); // opened explicitly: show it even with an unknown extension
        }
        sort_by_name(&mut files);
        if files.is_empty() {
            return None;
        }
        let index = start.and_then(|s| files.iter().position(|f| *f == s)).unwrap_or(0);
        Some(FileList { files, index })
    }

    /// Sorts by name (natural order), date modified, file size or randomly; keeps the current file.
    pub fn sort(&mut self, order: usize, reverse: bool) {
        let current = self.files[self.index].clone();
        match order {
            1 | 2 => {
                let key = |p: &PathBuf| {
                    let m = std::fs::metadata(p).ok();
                    if order == 1 {
                        m.and_then(|m| m.modified().ok())
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_nanos())
                            .unwrap_or(0)
                    } else {
                        m.map(|m| m.len() as u128).unwrap_or(0)
                    }
                };
                // name order first, so equal keys stay in name order (the sort is stable)
                sort_by_name(&mut self.files);
                self.files.sort_by_cached_key(key);
            }
            3 => {
                // shuffle (xorshift, seeded by the clock)
                let mut x =
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1) | 1;
                for i in (1..self.files.len()).rev() {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    self.files.swap(i, (x % (i as u64 + 1)) as usize);
                }
            }
            _ => sort_by_name(&mut self.files),
        }
        if reverse && order != 3 {
            self.files.reverse();
        }
        self.index = self.files.iter().position(|p| *p == current).unwrap_or(0);
    }

    /// Takes a new listing of the folder: files still there keep their order, new ones are added
    /// at the end. Stays on the current file, or on the one after it when it is gone. Returns
    /// whether anything changed.
    pub fn merge(&mut self, fresh: Vec<PathBuf>) -> bool {
        let present: std::collections::HashSet<&PathBuf> = fresh.iter().collect();
        let old: std::collections::HashSet<&PathBuf> = self.files.iter().collect();
        let added: Vec<PathBuf> = fresh.iter().filter(|p| !old.contains(p)).cloned().collect();
        if added.is_empty() && self.files.iter().all(|p| present.contains(p)) {
            return false;
        }
        let current = self.current().to_path_buf();
        let before_current = self.files[..self.index].iter().filter(|p| present.contains(p)).count();
        let mut files: Vec<PathBuf> = self.files.iter().filter(|p| present.contains(p)).cloned().collect();
        files.extend(added);
        if files.is_empty() {
            return false; // nothing left: keep showing what was there
        }
        self.index = files.iter().position(|p| *p == current).unwrap_or(before_current.min(files.len() - 1));
        self.files = files;
        true
    }

    /// Lists the folder (or comic archive) of the current image again, staying on that image.
    pub fn reload(&self) -> Option<FileList> {
        let current = self.current();
        let source = crate::archive::archive_of(current).unwrap_or_else(|| current.to_path_buf());
        let mut list = FileList::new(&source)?;
        if let Some(i) = list.files.iter().position(|p| p == current) {
            list.index = i;
        } else if source != current {
            list.files.push(current.to_path_buf()); // hidden page that was being shown
            sort_by_path(&mut list.files);
            list.index = list.files.iter().position(|p| p == current).unwrap_or(0);
        }
        Some(list)
    }

    pub fn current(&self) -> &Path {
        &self.files[self.index]
    }
}

/// A string that sorts in natural order
#[derive(PartialEq, Eq)]
struct Natural(String);

impl Ord for Natural {
    fn cmp(&self, other: &Self) -> Ordering {
        natural_cmp(&self.0, &other.0)
    }
}

impl PartialOrd for Natural {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Natural order of file names; each name is lowercased once, not in every comparison.
fn sort_by_name(files: &mut [PathBuf]) {
    files.sort_by_cached_key(|p| Natural(file_name(p)));
}

/// Natural order of whole paths (pages of an archive, which may be in folders)
fn sort_by_path(files: &mut [PathBuf]) {
    files.sort_by_cached_key(|p| Natural(p.to_string_lossy().to_lowercase()));
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// Compares strings with runs of digits compared by value.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.as_bytes(), b.as_bytes());
    loop {
        match (a.first(), b.first()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let na = a.iter().take_while(|c| c.is_ascii_digit()).count();
                let nb = b.iter().take_while(|c| c.is_ascii_digit()).count();
                let (da, db) = (trim_zeros(&a[..na]), trim_zeros(&b[..nb]));
                let o = da.len().cmp(&db.len()).then_with(|| da.cmp(db)).then_with(|| na.cmp(&nb));
                if o != Ordering::Equal {
                    return o;
                }
                a = &a[na..];
                b = &b[nb..];
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(y);
                }
                a = &a[1..];
                b = &b[1..];
            }
        }
    }
}

fn trim_zeros(d: &[u8]) -> &[u8] {
    let n = d.iter().take_while(|c| **c == b'0').count();
    &d[n.min(d.len().saturating_sub(1))..]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn natural_order() {
        let mut v = vec!["img10.png", "img2.png", "img1.png", "a.png", "img02.png"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["a.png", "img1.png", "img2.png", "img02.png", "img10.png"]);
    }

    #[test]
    fn hidden_images() {
        let dir = std::env::temp_dir().join(format!("lumaris-test-hidden-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["a.png", ".b.png", "c.jpg"] {
            std::fs::write(dir.join(f), b"").unwrap();
        }
        let names = |l: FileList| l.files.iter().map(|p| file_name(p.as_path())).collect::<Vec<_>>();
        assert_eq!(names(FileList::list(&dir, true).unwrap()), ["a.png", "c.jpg"]);
        assert_eq!(names(FileList::list(&dir, false).unwrap()), [".b.png", "a.png", "c.jpg"]);
        // opened explicitly: shown even though hidden
        let l = FileList::list(&dir.join(".b.png"), true).unwrap();
        assert_eq!(file_name(l.current()), ".b.png");
        assert_eq!(l.files.len(), 3);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn merge_listing() {
        let p = |n: &str| PathBuf::from(format!("/f/{n}"));
        let mut l = FileList { files: vec![p("a"), p("b"), p("c")], index: 1 };
        assert!(!l.merge(vec![p("c"), p("a"), p("b")]));
        // b deleted, d added: stays in place, on the image after b
        assert!(l.merge(vec![p("a"), p("c"), p("d")]));
        assert_eq!(l.files, [p("a"), p("c"), p("d")]);
        assert_eq!(l.current(), p("c"));
        // d renamed to e while on c
        assert!(l.merge(vec![p("a"), p("c"), p("e")]));
        assert_eq!(l.files, [p("a"), p("c"), p("e")]);
        assert_eq!(l.current(), p("c"));
    }

    #[test]
    fn sorting_by_name() {
        let mut v: Vec<PathBuf> = ["B10.png", "b2.png", "a.png"].iter().map(PathBuf::from).collect();
        sort_by_name(&mut v);
        assert_eq!(v, [PathBuf::from("a.png"), PathBuf::from("b2.png"), PathBuf::from("B10.png")]);
    }
}
