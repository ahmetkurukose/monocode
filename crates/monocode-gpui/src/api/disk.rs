//! Keeping the view in step with the files: what was last read, what changed
//! outside, and the tabs that come back with their project.

use super::*;

impl ApiView {
    /// Size and modification time of every collection and environment file
    /// of the shown projects.
    pub(super) fn disk_state(&self) -> HashMap<PathBuf, (SystemTime, u64)> {
        let mut state = HashMap::new();
        for project in self.shown_projects() {
            let Ok(entries) = std::fs::read_dir(api_dir(&project)) else {
                continue;
            };
            for entry in entries.flatten() {
                let file = entry.path();
                let name = file.to_string_lossy();
                if !(name.ends_with(COLLECTION_SUFFIX) || name.ends_with(ENVIRONMENT_SUFFIX)) {
                    continue;
                }
                if let Ok(meta) = entry.metadata() {
                    state.insert(
                        file,
                        (
                            meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                            meta.len(),
                        ),
                    );
                }
            }
        }
        state
    }

    /// Records a file this view just wrote, so its own write is not taken
    /// for an outside change.
    pub(super) fn touch(&mut self, file: &Path) {
        match std::fs::metadata(file) {
            Ok(meta) => {
                self.fingerprints.insert(
                    file.to_owned(),
                    (
                        meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                        meta.len(),
                    ),
                );
            }
            Err(_) => {
                self.fingerprints.remove(file);
            }
        }
    }

    /// Reads the files again when something other than this view changed
    /// them. Returns whether it did.
    pub(super) fn refresh_from_disk(&mut self, cx: &mut Context<Self>) -> bool {
        if self.disk_state() == self.fingerprints {
            return false;
        }
        self.reload(cx);
        true
    }

    /// Runs after every read of the files: records what was read, settles
    /// the open tabs against it, brings back saved tabs whose project is
    /// shown, and watches the folders.
    pub(super) fn after_reload(&mut self, cx: &mut Context<Self>) {
        self.fingerprints = self.disk_state();
        let mut refresh: Vec<(u64, ApiRequest)> = Vec::new();
        let mut gone: Vec<(u64, bool)> = Vec::new();
        for tab in &self.tabs {
            match self.collection(&tab.file) {
                Some(loaded) => match loaded.collection.request(&tab.path) {
                    // A tab with nothing typed follows the file.
                    Some(request) if !tab.dirty() && request != tab.saved => {
                        refresh.push((tab.id, request))
                    }
                    Some(_) => {}
                    // A request deleted under a tab with edits keeps the tab.
                    None if !tab.dirty() => gone.push((tab.id, false)),
                    None => {}
                },
                // The collection is not shown now (another project): the tab
                // waits for it, unless the file is gone.
                None => gone.push((tab.id, tab.file.exists())),
            }
        }
        for (id, request) in refresh {
            if let Some(tab) = self.tab_mut(id) {
                tab.draft = request.clone();
                tab.saved = request;
            }
            self.refresh_inputs(id, cx);
        }
        for (id, park) in gone {
            if let Some(index) = self.tabs.iter().position(|t| t.id == id) {
                let tab = self.tabs.remove(index);
                if park {
                    self.parked.push(SavedTab {
                        file: tab.file.to_string_lossy().into_owned(),
                        path: tab.path,
                    });
                }
            }
        }
        if self.active.is_some_and(|id| self.tab(id).is_none()) {
            self.active = self.tabs.last().map(|t| t.id);
        }
        self.restore_parked(cx);
        self.rewatch(cx);
        cx.notify();
    }

    /// Reopens saved tabs whose collection is shown now.
    fn restore_parked(&mut self, cx: &mut Context<Self>) {
        if self.parked.is_empty() {
            return;
        }
        let mut waiting = Vec::new();
        let mut front = None;
        for saved in std::mem::take(&mut self.parked) {
            let file = PathBuf::from(&saved.file);
            match self.collection(&file) {
                Some(loaded) if loaded.collection.request(&saved.path).is_some() => {
                    let open = self
                        .tabs
                        .iter()
                        .find(|t| t.file == file && t.path == saved.path)
                        .map(|t| t.id);
                    let id = open.or_else(|| self.add_tab(&file, saved.path.clone(), cx));
                    if self.restore_active.as_ref() == Some(&saved) {
                        front = id;
                    }
                }
                // Its collection is here and the request is not: dropped.
                Some(_) => {}
                None if file.exists() => waiting.push(saved),
                None => {}
            }
        }
        self.parked = waiting;
        if let Some(id) = front {
            self.active = Some(id);
            self.restore_active = None;
        } else if self.active.is_none() {
            self.active = self.tabs.last().map(|t| t.id);
        }
    }

    /// Watches the shown projects' API folders for outside changes.
    fn rewatch(&mut self, cx: &mut Context<Self>) {
        self.watch = None;
        self._watch_task = None;
        if cx.is_test() {
            return;
        }
        let dirs: Vec<PathBuf> = self.shown_projects().iter().map(|p| api_dir(p)).collect();
        let Some((watch, changes)) = crate::fs_watch::watch(&dirs) else {
            return;
        };
        self.watch = Some(watch);
        self._watch_task = Some(cx.spawn(async move |this, cx| {
            while changes.recv().await.is_ok() {
                // A save is several events; wait for them to settle.
                cx.background_executor()
                    .timer(Duration::from_millis(350))
                    .await;
                while changes.try_recv().is_ok() {}
                if this
                    .update(cx, |this, cx| {
                        this.refresh_from_disk(cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }
}
