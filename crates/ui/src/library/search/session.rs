use super::*;

fn search_result_matches(
    kind: RecentSearchKind,
    media_uri: &str,
    current: &crate::widgets::mounted_route::RouteCurrentTrack,
) -> bool {
    use rufin_core::collection_context::CollectionTarget;
    let target = match kind {
        RecentSearchKind::Track => return media_uri == current.media_uri,
        RecentSearchKind::Album => CollectionTarget::Album(media_uri.to_owned()),
        RecentSearchKind::Artist => CollectionTarget::Artist {
            id: media_uri.to_owned(),
            album_artist: false,
        },
    };
    current.context.as_ref().is_some_and(|context| {
        rufin_core::collection_context::plays_context(&target.context_id(), &context.context_id)
    })
}

pub struct SearchPreview {
    pub title: String,
    pub subtitle: String,
    pub artwork: ArtworkBinding,
    pub lyrics: Option<library::LyricsSnippet>,
}

/// How many results the header's dropdown shows; the Search page lists every one.
pub const SEARCH_PREVIEW_LIMIT: usize = 30;

/// What a Search page found and the first rows of each category.
type Found = (Arc<rufin_core::view::View>, [Vec<SearchItem>; 3]);

/// The Search page `view` with the first window of each category's rows.
async fn found(
    view: Arc<rufin_core::view::View>,
    cancel: &ReadCancellation,
) -> library::LibraryResult<Found> {
    let mut seeds = <[Vec<SearchItem>; 3]>::default();
    for category in CollectionCategory::ALL {
        let rows = view
            .search_rows(category, 0..crate::widgets::view_model::WINDOW, cancel)
            .await?;
        seeds[category as usize] = search_items(rows);
    }
    Ok((view, seeds))
}

/// How many rows `category` lists: the results found so far, and one more that stands for
/// the server's next page while it may hold more.
fn listed(view: &rufin_core::view::View, category: CollectionCategory) -> usize {
    let (len, more) = view.search_len(category);
    len + usize::from(more)
}

/// One current query shared by the header and the Search route.
pub struct SearchSession {
    source: rufin_core::runtime::SourceHandle,
    runtime: tokio::runtime::Handle,
    /// The Search page that found the shown results; it says what a committed change means
    /// for them, and their rows read their windows from it.
    view: Rc<RefCell<Option<Arc<rufin_core::view::View>>>>,
    /// The library the search runs or waits in.
    searched_in: RefCell<Option<rufin_core::runtime::SelectedLibrary>>,
    query: RefCell<String>,
    mode: Cell<library::SearchMode>,
    /// The results by category, which the header and the Search page show.
    pub(super) rows: [Rows<SearchItem>; 3],
    /// The rows the header shows, held so their window stays read wherever the page scrolls.
    previewed: RefCell<Vec<RowItem>>,
    status: Cell<&'static str>,
    pub(super) revision: Cell<u64>,
    generation: Cell<u64>,
    debounce: RefCell<Option<glib::SourceId>>,
    cancellation: RefCell<Option<ReadCancellation>>,
    observers: RefCell<Vec<Weak<dyn Fn()>>>,
    pub(super) category: Cell<CollectionCategory>,
}

impl SearchSession {
    /// Shows what the Search page `view` found, `seeds` its first rows. The same search read
    /// again keeps its rows in place until their windows are read, so their selection stays.
    fn show_found(&self, view: Option<Arc<rufin_core::view::View>>, seeds: [Vec<SearchItem>; 3]) {
        let same = match (&view, &*self.view.borrow()) {
            (Some(view), Some(previous)) => view.same_list(previous),
            _ => false,
        };
        self.view.replace(view.clone());
        for (category, seed) in CollectionCategory::ALL.into_iter().zip(seeds) {
            let len = view.as_ref().map_or(0, |view| listed(view, category));
            self.rows[category as usize].replace(len, 0, seed, same);
        }
        self.previewed.replace(
            self.rows
                .iter()
                .flat_map(|rows| {
                    let list = rows.list_model();
                    (0..SEARCH_PREVIEW_LIMIT.min(rows.len()) as u32)
                        .filter_map(move |position| list.item(position).and_downcast())
                })
                .collect(),
        );
    }

    /// Takes the length of `category` after a read of its rows, which may have found the
    /// server's next page or the end of its results.
    fn take_len(&self, category: CollectionCategory) {
        let Some(view) = self.view.borrow().clone() else {
            return;
        };
        let rows = &self.rows[category as usize];
        let len = listed(&view, category);
        if rows.len() == len {
            return;
        }
        let emptied = rows.is_empty() != (len == 0);
        rows.replace(len, 0, Vec::new(), true);
        if emptied {
            self.revision.set(self.revision.get().wrapping_add(1));
            self.notify();
        }
    }

    /// The read result at `index` of `category`.
    fn item(&self, category: CollectionCategory, index: usize) -> Option<Arc<SearchItem>> {
        self.rows[category as usize].ready(index)
    }

    pub fn new(
        source: rufin_core::runtime::SourceHandle,
        runtime: tokio::runtime::Handle,
    ) -> Rc<Self> {
        let view = Rc::new(RefCell::new(None::<Arc<rufin_core::view::View>>));
        let rows = CollectionCategory::ALL.map(|category| {
            let view = Rc::clone(&view);
            Rows::paged(
                runtime.clone(),
                0,
                Rc::new(move |range, cancel| {
                    let view = view.borrow().clone();
                    Box::pin(async move {
                        let Some(view) = view else {
                            return Ok(Vec::new());
                        };
                        view.search_rows(category, range, &cancel)
                            .await
                            .map(search_items)
                            .map_err(|error| error.to_string())
                    })
                }),
            )
        });
        Rc::new_cyclic(|session: &Weak<Self>| {
            for (category, rows) in CollectionCategory::ALL.into_iter().zip(&rows) {
                let session = session.clone();
                rows.on_read(Rc::new(move || {
                    if let Some(session) = session.upgrade() {
                        session.take_len(category);
                    }
                }));
            }
            Self {
                source,
                runtime,
                view,
                searched_in: RefCell::new(None),
                query: RefCell::new(String::new()),
                mode: Cell::new(library::SearchMode::Title),
                rows,
                previewed: Default::default(),
                status: Cell::new("initial"),
                revision: Cell::new(0),
                generation: Cell::new(0),
                debounce: RefCell::new(None),
                cancellation: RefCell::new(None),
                observers: RefCell::new(Vec::new()),
                category: Cell::new(CollectionCategory::default()),
            }
        })
    }
}

impl SearchSession {
    /// Plays a recent search result at `placement` as core plays it outside a page.
    pub fn play_recent(
        result: &RecentSearchResult,
        catalog: &Rc<CatalogUi>,
        placement: QueuePlacement,
    ) {
        catalog
            .settings
            .update_app_settings("recent search", |settings| {
                settings.remember_search_result(result.clone())
            });
        let media_uri = result.media_uri.clone();
        let source = catalog.products.source.clone();
        let playback = catalog.products.playback.clone();
        catalog.products.runtime.spawn(async move {
            let selected = source.selected_library();
            let cancel = ReadCancellation::new();
            match source
                .play_uri(&media_uri, selected.as_ref(), placement, false, &cancel)
                .await
            {
                Ok(request) => playback.play_selection(request),
                Err(error) => tracing::warn!(%error, "could not play the recent search"),
            }
        });
    }

    /// Opens a recent album or artist result; a track plays.
    pub fn activate_recent(result: &RecentSearchResult, catalog: &Rc<CatalogUi>) {
        let Some(route) = Route::of_uri(&result.media_uri) else {
            Self::play_recent(result, catalog, QueuePlacement::Now);
            return;
        };
        catalog
            .settings
            .update_app_settings("recent search", |settings| {
                settings.remember_search_result(result.clone())
            });
        catalog.open_route(route);
    }

    pub fn observe(&self, callback: impl Fn() + 'static) -> Rc<dyn Fn()> {
        let callback: Rc<dyn Fn()> = Rc::new(callback);
        self.observers.borrow_mut().push(Rc::downgrade(&callback));
        callback
    }

    fn notify(&self) {
        let callbacks: Vec<_> = self
            .observers
            .borrow()
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        self.observers
            .borrow_mut()
            .retain(|observer| observer.strong_count() != 0);
        for callback in callbacks {
            callback();
        }
    }

    pub fn query(&self) -> String {
        self.query.borrow().clone()
    }

    pub fn mode(&self) -> library::SearchMode {
        self.mode.get()
    }

    pub fn set_mode(self: &Rc<Self>, mode: library::SearchMode) {
        if self.mode.replace(mode) == mode {
            return;
        }
        if mode == library::SearchMode::Lyrics {
            self.category.set(CollectionCategory::Tracks);
        }
        self.schedule();
    }

    pub fn status(&self) -> &'static str {
        self.status.get()
    }

    pub fn set_category(&self, category: CollectionCategory) {
        self.category.set(category);
        self.notify();
    }

    pub fn set_query(self: &Rc<Self>, query: &str) {
        let query = query.trim();
        if *self.query.borrow() == query {
            return;
        }
        self.query.replace(query.to_owned());
        self.schedule();
    }

    fn cancel(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
        if let Some(source) = self.debounce.borrow_mut().take() {
            source.remove();
        }
        if let Some(cancellation) = self.cancellation.borrow_mut().take() {
            cancellation.cancel();
        }
    }

    pub fn refresh(self: &Rc<Self>) {
        self.schedule();
        self.submit();
    }

    fn schedule(self: &Rc<Self>) {
        self.cancel();
        self.searched_in.replace(self.source.selected_library());
        self.show_found(None, Default::default());
        self.revision.set(self.revision.get().wrapping_add(1));
        if self.query.borrow().is_empty()
            || (rufin_core::route::Route::Search.requires_selected_source(self.mode())
                && self.source.selected_library().is_none())
            || (self.mode() == library::SearchMode::Lyrics
                && self.query.borrow().chars().nth(1).is_none())
        {
            self.status.set("initial");
        } else {
            self.status.set("loading");
            let weak = Rc::downgrade(self);
            self.debounce.replace(Some(glib::timeout_add_local_once(
                SEARCH_DEBOUNCE,
                move || {
                    if let Some(session) = weak.upgrade() {
                        session.debounce.borrow_mut().take();
                        session.submit();
                    }
                },
            )));
        }
        self.notify();
    }

    /// Enter also submits a one-character lyric query, but never repeats a running or finished query.
    pub fn submit(self: &Rc<Self>) {
        let single_lyric = self.mode() == library::SearchMode::Lyrics
            && self.status.get() == "initial"
            && !self.query.borrow().is_empty();
        if (self.status.get() != "loading" && !single_lyric) || self.cancellation.borrow().is_some()
        {
            return;
        }
        if let Some(source) = self.debounce.borrow_mut().take() {
            source.remove();
        }
        let generation = self.generation.get();
        let cancellation = ReadCancellation::new();
        self.cancellation.replace(Some(cancellation.clone()));
        if single_lyric {
            self.status.set("loading");
            self.revision.set(self.revision.get().wrapping_add(1));
            self.notify();
        }
        let options = rufin_core::view::OpenOptions {
            filter: self.query(),
            search_mode: self.mode(),
            ..Default::default()
        };
        let source = self.source.clone();
        let reading = self.source.reading();
        let selected = self.source.selected_library();
        let task = self.runtime.spawn(async move {
            let opened = source
                .open(
                    Route::Search,
                    selected,
                    options,
                    library::RouteSeedWindow::top(),
                    true,
                    &cancellation,
                )
                .await?;
            found(opened.view, &cancellation).await
        });
        self.show(task, reading, generation);
    }

    /// Handles a committed change as the Search page that found the results says: shows it
    /// in them, reads them again keeping them shown, or searches again in the library
    /// selected now. A search still running hears of the change from its own read.
    pub fn apply_changed(self: &Rc<Self>, changed: &rufin_core::source::feed::Changed) {
        let view = self.view.borrow().clone();
        for rows in &self.rows {
            rows.show_changes(&self.source, changed, view.as_deref());
        }
        let current = self.source.selected_library();
        let Some(view) = view else {
            // A search that found nothing yet, such as one waiting for a source, runs again
            // when core says the Search page opens again.
            let searched_in = self.searched_in.borrow().clone();
            if rufin_core::view::refresh_unread(
                &Route::Search,
                searched_in.as_ref(),
                current.as_ref(),
                changed,
            ) == rufin_core::view::Refresh::Reopen
            {
                self.refresh();
            }
            return;
        };
        match view.refresh(current.as_ref(), changed) {
            rufin_core::view::Refresh::Nothing => {}
            rufin_core::view::Refresh::Reread => self.reread(view),
            rufin_core::view::Refresh::Reopen => self.refresh(),
        }
    }

    /// Reads the Search page `view` again, keeping its results shown until then.
    fn reread(self: &Rc<Self>, view: Arc<rufin_core::view::View>) {
        self.cancel();
        let generation = self.generation.get();
        let cancellation = ReadCancellation::new();
        self.cancellation.replace(Some(cancellation.clone()));
        let reading = self.source.reading();
        let task = self.runtime.spawn(async move {
            let opened = view
                .reread(
                    view.options().clone(),
                    library::RouteSeedWindow::top(),
                    &cancellation,
                )
                .await?;
            found(opened.view, &cancellation).await
        });
        self.show(task, reading, generation);
    }

    /// Shows what the Search page `task` reads, unless another read started since, with what
    /// changed while it read.
    fn show(
        self: &Rc<Self>,
        task: tokio::task::JoinHandle<library::LibraryResult<Found>>,
        reading: rufin_core::shown::Reading,
        generation: u64,
    ) {
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = task
                .await
                .map_err(|error| error.to_string())
                .and_then(|found| found.map_err(|error| error.to_string()));
            let Some(session) = weak.upgrade() else {
                return;
            };
            if session.generation.get() != generation {
                return;
            }
            session.cancellation.borrow_mut().take();
            match result {
                Ok((view, seeds)) => {
                    session.show_found(Some(view), seeds);
                    session.status.set("results");
                    // What committed while it searched reaches the results as any other change.
                    if let Some(missed) = reading.missed() {
                        session.apply_changed(&missed);
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "failed to load Search results");
                    session.status.set("error");
                }
            }
            session.revision.set(session.revision.get().wrapping_add(1));
            session.notify();
        });
    }

    pub fn previews(&self) -> [Vec<SearchPreview>; 3] {
        // The rows read from the top; the one that stands for a next page is not one.
        let lengths = self.rows.each_ref().map(|rows| {
            (0..SEARCH_PREVIEW_LIMIT)
                .take_while(|position| rows.ready(*position).is_some())
                .count()
        });
        let mut counts = [0; 3];
        let mut total = 0;
        while total < SEARCH_PREVIEW_LIMIT {
            let previous = total;
            for category in [
                CollectionCategory::Tracks,
                CollectionCategory::Tracks,
                CollectionCategory::Albums,
                CollectionCategory::Artists,
            ] {
                let index = category as usize;
                if total < SEARCH_PREVIEW_LIMIT && counts[index] < lengths[index] {
                    counts[index] += 1;
                    total += 1;
                }
            }
            if total == previous {
                break;
            }
        }
        std::array::from_fn(|index| {
            (0..counts[index])
                .filter_map(|position| self.item(CollectionCategory::ALL[index], position))
                .map(|item| SearchPreview {
                    title: item.title().to_owned(),
                    subtitle: item.subtitle().to_owned(),
                    artwork: item.artwork(),
                    lyrics: item.lyrics().cloned(),
                })
                .collect()
        })
    }

    pub fn activate(&self, category: CollectionCategory, index: usize, catalog: &Rc<CatalogUi>) {
        let item = self.item(category, index);
        if let Some(item) = item {
            item.activate(catalog);
        }
    }

    /// Plays result `row` at `placement` as the Search page that found it plays its rows.
    pub(super) fn play_row(
        &self,
        row: library::Row,
        placement: QueuePlacement,
        catalog: &CatalogUi,
    ) {
        let Some(view) = self.view.borrow().clone() else {
            return;
        };
        let playback = catalog.products.playback.clone();
        self.runtime.spawn(async move {
            let cancel = ReadCancellation::new();
            match view.play(&row, 0, placement, false, &cancel).await {
                Ok(Some(request)) => playback.play_selection(request),
                Ok(None) => {}
                Err(error) => tracing::warn!(%error, "could not play the search result"),
            }
        });
    }

    pub fn play(&self, category: CollectionCategory, index: usize, catalog: &Rc<CatalogUi>) {
        let item = self.item(category, index);
        if let Some(item) = item {
            item.play(catalog, QueuePlacement::Now);
        }
    }

    pub fn preview_matches(
        &self,
        category: CollectionCategory,
        index: usize,
        current: &crate::widgets::mounted_route::RouteCurrentTrack,
    ) -> bool {
        self.item(category, index).is_some_and(|item| {
            let (kind, uri) = match &*item {
                SearchItem::Track(row, _) => (RecentSearchKind::Track, row.media_uri.as_str()),
                SearchItem::Album(row) => (RecentSearchKind::Album, row.media_uri.as_str()),
                SearchItem::Artist(row) => (RecentSearchKind::Artist, row.media_uri.as_str()),
            };
            search_result_matches(kind, uri, current)
        })
    }

    pub fn recent_matches(
        result: &RecentSearchResult,
        current: &crate::widgets::mounted_route::RouteCurrentTrack,
    ) -> bool {
        search_result_matches(result.kind, &result.media_uri, current)
    }

    pub fn present_context(
        self: &Rc<Self>,
        category: CollectionCategory,
        index: usize,
        target: &gtk::Widget,
        catalog: &Rc<CatalogUi>,
    ) {
        if let Some(item) = self.item(category, index) {
            item.present_context(target, catalog, None);
        }
    }
}

impl Drop for SearchSession {
    fn drop(&mut self) {
        self.cancel();
    }
}
