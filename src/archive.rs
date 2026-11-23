// Comic archives (.cbz, .zip): their images are shown like the images of a folder. A page is
// addressed as <archive path>/<name inside the archive>.
use std::path::{Path, PathBuf};

pub fn is_archive(path: &Path) -> bool {
    path.is_file()
        && path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("cbz") || e.eq_ignore_ascii_case("zip"))
}

/// The images in the archive (names inside the archive)
pub fn images(archive: &Path) -> Option<Vec<String>> {
    let zip = zip::ZipArchive::new(std::fs::File::open(archive).ok()?).ok()?;
    Some(
        zip.file_names()
            .filter_map(Result::ok)
            .filter(|n| !n.ends_with('/') && !n.starts_with("__MACOSX/"))
            .filter(|n| crate::files::is_supported(Path::new(n.as_ref())))
            .map(String::from)
            .collect(),
    )
}

/// The archive a page path belongs to
pub fn archive_of(page: &Path) -> Option<PathBuf> {
    if page.exists() {
        return None;
    }
    page.ancestors().skip(1).find(|a| is_archive(a)).map(Path::to_path_buf)
}

/// The bytes of a page. None: not a page of an archive.
pub fn read(page: &Path) -> Option<Result<Vec<u8>, String>> {
    let archive = archive_of(page)?;
    let name = page.strip_prefix(&archive).ok()?.to_string_lossy().replace('\\', "/");
    Some((|| {
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&archive).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let mut entry = zip.by_name(&name).map_err(|e| e.to_string())?;
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        std::io::Read::read_to_end(&mut entry, &mut bytes).map_err(|e| e.to_string())?;
        Ok(bytes)
    })())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comic_archive() {
        let archive = std::fs::canonicalize("tests/data/comic.cbz").unwrap();
        assert!(is_archive(&archive));
        // images only: no folders, macOS resource files or text
        let mut names = images(&archive).unwrap();
        names.sort();
        assert_eq!(names, ["pages/page10.jpg", "pages/page2.jpg"]);
        let page = archive.join("pages/page2.jpg");
        assert_eq!(archive_of(&page), Some(archive.clone()));
        let bytes = read(&page).unwrap().unwrap();
        assert_eq!(bytes, std::fs::read("tests/data/restart-444.jpg").unwrap());
        assert!(read(&archive.join("pages/missing.jpg")).unwrap().is_err());
    }
}
