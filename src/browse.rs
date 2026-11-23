// Browsing: the images of the folder, decoding ahead, the folder watch, slideshow, trash.
use crate::*;

impl App {
    pub(crate) fn move_to_trash(&mut self, event_loop: &ActiveEventLoop) {
        if self.current.is_none() {
            return;
        }
        let path = self.files.current().to_path_buf();
        if archive::archive_of(&path).is_some() {
            if let Some(w) = &self.window {
                w.set_title(&Self::title_for(&path, " (pages of an archive cannot be moved to the trash)"));
            }
            return;
        }
        match trash::trash(&path) {
            Ok(()) => {
                self.cache.retain(|(p, _)| *p != path);
                let i = self.files.index;
                self.files.files.remove(i);
                if self.files.files.is_empty() {
                    event_loop.exit();
                    return;
                }
                self.files.index = i.min(self.files.files.len() - 1);
                self.shown = None;
                let k = self.files.index;
                self.files.index = usize::MAX; // force go_to to show it
                self.go_to_forced(event_loop, k);
            }
            Err(e) => {
                eprintln!("lumaris: cannot move {} to the trash: {e}", path.display());
                if let Some(w) = &self.window {
                    w.set_title(&Self::title_for(&path, &format!(" (not moved to the trash: {e})")));
                }
            }
        }
    }

    pub(crate) fn go_to_forced(&mut self, event_loop: &ActiveEventLoop, index: usize) {
        self.files.index = index;
        let path = self.files.current().to_path_buf();
        if let Some((_, img)) = self.cache.iter().find(|(p, _)| *p == path) {
            let img = img.clone();
            self.show(event_loop, path, img);
        }
        self.prefetch();
    }

    /// Opens another image (and its folder)
    pub(crate) fn open_path(&mut self, event_loop: &ActiveEventLoop, path: &Path) {
        let Some(files) = files::FileList::new(path) else {
            eprintln!("lumaris: no images found: {}", path.display());
            return;
        };
        self.files = files;
        let s = settings::current().clone();
        if s.choice("SortOrder") != 0 || s.bool("SortReverse") {
            self.files.sort(s.choice("SortOrder"), s.bool("SortReverse"));
        }
        self.cache.clear();
        self.shown = None;
        self.stop_slideshow();
        self.watch_folder();
        let k = self.files.index;
        self.go_to_forced(event_loop, k);
    }

    /// Watches the folder of the current image (not the pages of an archive) if enabled
    pub(crate) fn watch_folder(&self) {
        let current = self.files.current();
        let folder = (settings::current().bool("WatchFolder") && archive::archive_of(current).is_none())
            .then(|| current.parent().map(Path::to_path_buf))
            .flatten();
        self.watch.set(folder);
    }

    /// Files were added to or removed from the watched folder: list it again.
    pub(crate) fn folder_changed(&mut self, event_loop: &ActiveEventLoop, dir: &Path) {
        let current = self.files.current().to_path_buf();
        if current.parent() != Some(dir) {
            return;
        }
        let mut fresh = files::FileList::new(dir).map(|l| l.files).unwrap_or_default();
        if current.exists() && !fresh.contains(&current) {
            fresh.push(current.clone()); // opened explicitly (a hidden image, unknown extension)
        }
        if !self.files.merge(fresh) {
            return;
        }
        let s = settings::current().clone();
        if s.choice("SortOrder") != 3 {
            self.files.sort(s.choice("SortOrder"), s.bool("SortReverse")); // random order: new images at the end
        }
        if self.files.current() != current {
            // the image shown was deleted or renamed: show the next one
            self.shown = None;
            let k = self.files.index;
            self.go_to_forced(event_loop, k);
        } else {
            self.prefetch();
            self.refresh_info();
            self.redraw();
        }
    }

    /// The open dialog (in the background); the chosen file arrives as UserEvent::Open
    pub(crate) fn open_dialog(&mut self) {
        let folder = self.files.files.get(self.files.index).and_then(|p| p.parent()).map(Path::to_path_buf);
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            if let Some(p) = portal::pick_image(folder.as_deref()) {
                let _ = proxy.send_event(UserEvent::Open(p));
            }
        });
    }

    pub(crate) fn go_to(&mut self, event_loop: &ActiveEventLoop, index: usize) {
        if index >= self.files.files.len() || (index == self.files.index && self.shown.is_some()) {
            return;
        }
        self.files.index = index;
        let path = self.files.current().to_path_buf();
        if let Some((_, img)) = self.cache.iter().find(|(p, _)| *p == path) {
            let img = img.clone();
            self.show(event_loop, path, img);
        }
        self.prefetch();
    }

    /// Asks the loader for the current image and its neighbours (nearest first) that are not
    /// decoded yet, as far as the memory limit allows, and drops images no longer needed.
    pub(crate) fn prefetch(&mut self) {
        self.trim_cache();
        let ahead = settings::current().number("Prefetch") as isize;
        let limit = Self::memory_limit();
        let mut used: usize = self.cache.iter().map(|(_, img)| Self::image_bytes(img)).sum();
        // neighbours are guessed to be the size of the current image
        let guess = self.current.as_ref().map(|img| Self::image_bytes(img)).unwrap_or(0);
        let mut wanted = vec![self.files.index];
        wanted.extend((1..=ahead).flat_map(|d| [self.neighbour(d), self.neighbour(-d)]).flatten());
        let mut paths: Vec<PathBuf> = Vec::new();
        for (n, k) in wanted.into_iter().enumerate() {
            let p = &self.files.files[k];
            if paths.contains(p) || self.cache.iter().any(|(c, _)| c == p) {
                continue;
            }
            if n > 0 {
                if used + guess > limit {
                    break;
                }
                used += guess;
            }
            paths.push(p.clone());
        }
        self.loader.want(paths);
    }

    pub(crate) fn memory_limit() -> usize {
        (settings::current().number("MemoryLimit") * 1024.0 * 1024.0) as usize
    }

    pub(crate) fn image_bytes(img: &Decoded) -> usize {
        img.rgba.len() + img.frames.iter().map(|f| f.rgba.len()).sum::<usize>()
    }

    /// Keeps the decoded images near the current one (the folder may loop), nearest first, within
    /// the memory limit. The current image always stays.
    pub(crate) fn trim_cache(&mut self) {
        let (i, n) = (self.files.index, self.files.files.len());
        let ahead = settings::current().number("Prefetch") as usize;
        let files = &self.files.files;
        let distance = |p: &PathBuf| {
            files.iter().position(|f| f == p).map(|k| {
                let d = k.abs_diff(i);
                d.min(n - d)
            })
        };
        let mut kept: Vec<(usize, (PathBuf, Arc<Decoded>))> =
            self.cache.drain(..).filter_map(|e| distance(&e.0).filter(|d| *d <= ahead + 1).map(|d| (d, e))).collect();
        kept.sort_by_key(|(d, _)| *d);
        let limit = Self::memory_limit();
        let mut used = 0;
        for (d, e) in kept {
            used += Self::image_bytes(&e.1);
            if d == 0 || used <= limit {
                self.cache.push(e);
            }
        }
    }

    /// Index `delta` images away, looping through the folder if enabled
    pub(crate) fn neighbour(&self, delta: isize) -> Option<usize> {
        let n = self.files.files.len() as isize;
        let k = self.files.index as isize + delta;
        if self.loop_folder { Some(k.rem_euclid(n) as usize) } else { (0..n).contains(&k).then_some(k as usize) }
    }

    pub(crate) fn step(&mut self, event_loop: &ActiveEventLoop, delta: isize) {
        if let Some(k) = self.neighbour(delta) {
            self.go_to(event_loop, k);
        }
    }

    pub(crate) fn start_slideshow(&mut self, interval: Duration) {
        if settings::current().bool("SlideshowFullScreen") && self.window.as_ref().is_some_and(|w| w.fullscreen().is_none()) {
            self.toggle_fullscreen();
        }
        self.slideshow = Some(interval);
        self.next_slide = Some(Instant::now() + interval);
    }

    pub(crate) fn stop_slideshow(&mut self) {
        self.slideshow = None;
        self.next_slide = None;
    }
}
