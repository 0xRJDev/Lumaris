// The images of a folder, in natural order ("img2" before "img10").
use std::cmp::Ordering;
use std::path::{Path, PathBuf};

const EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "apng", "gif", "webp", "bmp", "dib", "tif", "tiff", "qoi",
    "ico", "tga", "pnm", "pbm", "pgm", "ppm", "pam",
];

pub fn is_supported(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

pub struct FileList {
    pub files: Vec<PathBuf>,
    pub index: usize,
}

impl FileList {
    /// `arg` is an image (its folder is listed) or a folder (starts at its first image).
    pub fn new(arg: &Path) -> Option<FileList> {
        let arg = std::fs::canonicalize(arg).ok()?;
        let (dir, start) = if arg.is_dir() { (arg.clone(), None) } else { (arg.parent()?.to_path_buf(), Some(arg)) };
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_file() || t.is_symlink()))
                    .map(|e| e.path())
                    .filter(|p| is_supported(p))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(s) = &start {
            if !files.contains(s) {
                files.push(s.clone()); // opened explicitly: show it even with an unknown extension
            }
        }
        files.sort_by(|a, b| natural_cmp(&file_name(a), &file_name(b)));
        if files.is_empty() {
            return None;
        }
        let index = start.and_then(|s| files.iter().position(|f| *f == s)).unwrap_or(0);
        Some(FileList { files, index })
    }

    pub fn current(&self) -> &Path {
        &self.files[self.index]
    }
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
}
