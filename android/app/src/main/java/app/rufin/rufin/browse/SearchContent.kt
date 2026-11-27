package app.rufin.rufin.browse

import app.rufin.rufin.shared.library.QueuePlacement
import app.rufin.rufin.shared.rufin_core.Route
import app.rufin.rufin.shared.library.SearchMode
import app.rufin.rufin.shared.rufin_core.RecentSearchKind
import app.rufin.rufin.shared.rufin_core.showsLyricsCachePrompt
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import app.rufin.rufin.ui.CoverShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.paging.PagingData
import androidx.paging.compose.collectAsLazyPagingItems
import app.rufin.rufin.core.translate
import app.rufin.rufin.app.RufinConnection
import app.rufin.rufin.player.PlayerConnection
import app.rufin.rufin.settings.PreferencesConnection
import app.rufin.rufin.shared.rufin_core.SourceOperation
import app.rufin.rufin.player.PlayerIconButton
import app.rufin.rufin.ui.Artwork
import app.rufin.rufin.ui.RufinIcon
import app.rufin.rufin.ui.LocalPlayerBottomPadding
import app.rufin.rufin.ui.rememberArtwork
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.flatMapLatest
import kotlinx.coroutines.flow.flowOf

@OptIn(ExperimentalFoundationApi::class, ExperimentalMaterial3Api::class)
@Composable
internal fun SearchContent(
    model: RufinConnection,
    preferences: PreferencesConnection,
    browse: BrowseConnection,
    player: PlayerConnection,
    onMenu: (BrowseItem) -> Unit,
    onPlaylist: (BrowseItem) -> Unit,
    onScrolled: (Boolean) -> Unit,
) {
    var wasDownloadingAssets by remember { mutableStateOf(false) }
    LaunchedEffect(model.sourceOperation) {
        val downloading = model.sourceOperation is SourceOperation.Downloading
        if (wasDownloadingAssets && !downloading && browse.section == AppSection.Search &&
            browse.searchMode == SearchMode.LYRICS && browse.searchText.isNotBlank()) {
            browse.submitSearch()
        }
        wasDownloadingAssets = downloading
    }
    Column(Modifier.fillMaxSize()) {
        if (browse.searchText.isNotBlank()) {
            app.rufin.rufin.ui.SegmentedPill(listOf(SearchMode.TITLE, SearchMode.LYRICS), browse.searchMode, { browse.searchMode = it },
                Modifier.padding(horizontal = 12.dp)) { mode, _ ->
                Text(translate(if (mode == SearchMode.TITLE) "By Title" else "By Lyrics"), style = MaterialTheme.typography.titleSmall, maxLines = 1)
            }
        }
        val settings = preferences.applicationSettings
        val showLyricsCachePrompt = browse.searchText.isNotBlank() &&
            browse.searchMode == SearchMode.LYRICS &&
            settings != null && showsLyricsCachePrompt(settings, model.sources?.sources.orEmpty())
        if (showLyricsCachePrompt) {
            Box(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp)) {
                Surface(
                    modifier = Modifier.fillMaxWidth(),
                    shape = MaterialTheme.shapes.large,
                    color = MaterialTheme.colorScheme.surfaceContainer,
                ) {
                    Column(
                        modifier = Modifier.fillMaxWidth().padding(start = 16.dp, top = 52.dp, end = 16.dp, bottom = 16.dp),
                        horizontalAlignment = Alignment.CenterHorizontally,
                        verticalArrangement = Arrangement.spacedBy(12.dp),
                    ) {
                        Text(
                            translate("Do you want to cache all lyrics automatically? You can also enable this from Preferences -> Storage"),
                            style = MaterialTheme.typography.bodyMedium,
                            textAlign = androidx.compose.ui.text.style.TextAlign.Center,
                        )
                        Button(onClick = {
                            preferences.changePreference { setDownloadLyricsAutomatically(true) }
                        }) {
                            Text(translate("Enable caching lyrics"))
                        }
                    }
                }
                IconButton(
                    onClick = {
                        preferences.changePreference { setLyricsCachePromptDismissed(true) }
                    },
                    modifier = Modifier.align(Alignment.TopEnd),
                ) {
                    RufinIcon("rufin-window-close-symbolic", translate("Close"))
                }
            }
        }
        Box(Modifier.weight(1f)) {
            SearchResults(browse, player, onMenu, onPlaylist, onScrolled)
        }
    }
}

/** How many results of each kind show while typing; a kind shown in full lists every one. */
private const val QUICK_RESULTS = 4

@OptIn(ExperimentalFoundationApi::class, ExperimentalMaterial3Api::class, kotlinx.coroutines.ExperimentalCoroutinesApi::class)
@Composable
private fun SearchResults(
    browse: BrowseConnection,
    player: PlayerConnection,
    onMenu: (BrowseItem) -> Unit,
    onPlaylist: (BrowseItem) -> Unit,
    onScrolled: (Boolean) -> Unit,
) {
    if (browse.searchText.isNotBlank()) {
        // Typing shows a few results of each kind under its heading, as the desktop's quick results do; Enter,
        // or a heading, shows one kind in full under tabs.
        LaunchedEffect(browse.searchText) { browse.searchSubmitted = false }
        val found = browse.searchKinds
        val kinds = remember(found, browse.categories) {
            browse.categories.mapNotNull { kind ->
                found.firstOrNull { it.category == kind.category && it.rows.isNotEmpty() }?.let { kind to it }
            }
        }
        if (kinds.isEmpty()) {
            if (browse.loading) app.rufin.rufin.ui.PageLoading() else BrowseEmptyState()
            return
        }
        if (!browse.searchSubmitted) {
            LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(bottom = 8.dp + LocalPlayerBottomPadding.current)) {
                kinds.forEach { (kind, results) ->
                    val rows = results.rows
                    item(key = "kind:${kind.category}") {
                        Row(Modifier.fillMaxWidth().clickable { browse.searchCategory = kind.category; browse.searchSubmitted = true }
                            .padding(start = 16.dp, end = 12.dp, top = 14.dp, bottom = 6.dp), verticalAlignment = Alignment.CenterVertically) {
                            Text(kind.title, Modifier.weight(1f), style = MaterialTheme.typography.titleMedium)
                            // A server that may hold more than it gave so far counts at least these.
                            Text(if (results.more) "${results.count}+" else results.count.toString(),
                                style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                            RufinIcon("rufin-go-next-symbolic", null, Modifier.padding(start = 4.dp).size(16.dp), MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                    }
                    items(rows.take(QUICK_RESULTS), key = { "${kind.category}:${it.row.sourceId.orEmpty()}:${it.row.key}:${it.index}" }) { item ->
                        val columns = item.row.fields.map { app.rufin.rufin.core.AndroidSortField(it.id, "") }
                        FieldRow(item, player, columns.any { it.id == app.rufin.rufin.shared.library.LibraryField.IMAGE ||
                            it.id == app.rufin.rufin.shared.library.LibraryField.TITLE_MERGED }, columns,
                            browse::open, onMenu, browse::favorite, onPlaylist)
                    }
                }
            }
            return
        }
        // A kind in full reads its rows as it scrolls, as a list page does.
        val kind = kinds.firstOrNull { it.first.category == browse.searchCategory } ?: kinds.first()
        Column(Modifier.fillMaxSize()) {
            app.rufin.rufin.ui.SegmentedPill(kinds.map { it.first.category }, kind.first.category, { browse.searchCategory = it },
                Modifier.padding(start = 12.dp, end = 12.dp, top = 8.dp, bottom = 4.dp), scroll = rememberScrollState()) { category, _ ->
                Text(kinds.first { it.first.category == category }.first.title, style = MaterialTheme.typography.titleSmall, maxLines = 1)
            }
            val category = kind.first.category
            val searchFlow = remember(browse, category) {
                snapshotFlow { browse.searchKinds.firstOrNull { it.category == category } }
                    .flatMapLatest { it?.let(browse::searchPages) ?: flowOf(PagingData.empty<BrowseItem>()) }
            }
            val rows = searchFlow.collectAsLazyPagingItems()
            Box(Modifier.weight(1f)) {
                BrowseContent(rows, emptyList(), player, false, false, false, browse::open, onMenu,
                    browse::favorite, onPlaylist, onScrolled,
                    loading = browse.loading, onRadio = { browse.radio(it, QueuePlacement.Replace(0UL)) })
            }
        }
        return
    }
    val state = rememberLazyListState()
    var selected by remember { mutableStateOf<RecentSearch?>(null) }
    var metadata by remember { mutableStateOf<BrowseItem?>(null) }
    LaunchedEffect(state) {
        snapshotFlow { state.firstVisibleItemIndex > 0 || state.firstVisibleItemScrollOffset > 0 }
            .distinctUntilChanged().collect(onScrolled)
    }
    // Nothing searched yet: the page says what it is for, in its middle, as on the desktop.
    if (browse.recentSearchRows.isEmpty()) {
        Box(Modifier.fillMaxSize().padding(bottom = LocalPlayerBottomPadding.current), contentAlignment = Alignment.Center) {
            Text(translate("Type to search"), style = MaterialTheme.typography.bodyLarge, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        return
    }
    LazyColumn(Modifier.fillMaxSize(), state = state, contentPadding = PaddingValues(bottom = 8.dp + LocalPlayerBottomPadding.current)) {
        item {
            Row(Modifier.fillMaxWidth().padding(start = 16.dp, end = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                Text(translate("Recent searches"), Modifier.weight(1f), style = MaterialTheme.typography.titleMedium)
                TextButton(browse::clearRecentSearches) { Text(translate("Clear")) }
            }
        }
        items(browse.recentSearchRows, key = { "${it.result.kind}:${it.result.mediaUri}" }) { recent ->
            // Drawn as saved, as GTK's search popup does; states, badge and menu come from its row.
            val (result, item) = recent.result to recent.item
            val pixels = with(LocalDensity.current) { 50.dp.roundToPx() }
            val artwork = rememberArtwork(result.artworkBinding, player, pixels)
            val row = @Composable {
            ListItem(
                headlineContent = { if (item != null) BrowseTitle(favoriteItem(player.model, item), player = player, title = result.title)
                    else Text(result.title, maxLines = 1, overflow = TextOverflow.Ellipsis) },
                supportingContent = {
                    val line = listOf(result.subtitle, translate(when (result.kind) {
                        RecentSearchKind.ALBUM -> "Album"
                        RecentSearchKind.ARTIST -> "Artist"
                        RecentSearchKind.TRACK -> "Track"
                    })).filter(String::isNotBlank).joinToString(" · ")
                    if (item != null) app.rufin.rufin.ui.DownloadedLine(item) { Text(line, maxLines = 1, overflow = TextOverflow.Ellipsis) }
                    else Text(line, maxLines = 1, overflow = TextOverflow.Ellipsis)
                },
                leadingContent = { Artwork(artwork, Modifier.size(50.dp).clip(CoverShape)) },
                trailingContent = if (item != null) null else {
                    { PlayerIconButton("rufin-user-trash-symbolic", "Remove", { browse.removeRecentSearch(result) }) }
                },
                modifier = Modifier.combinedClickable(onClick = { browse.openRecent(recent) }, onLongClick = { selected = recent }),
            )
            }
            if (item != null) BrowseSwipeActions(item, true, row) else row()
        }
    }
    selected?.let { recent -> recent.item?.let { item ->
        BrowseMenuSheet(player.model, browse, item, player, { selected = null },
            { selected = null; onPlaylist(item) }, { metadata = item; selected = null },
            { browse.removeRecentSearch(recent.result); selected = null },
            onNavigate = { route, title, _ -> selected = null; browse.openRoute(route, title) })
    } }
    metadata?.row?.metadataItem?.let { item -> app.rufin.rufin.metadata.MetadataEditorScreen(player.model, item) { metadata = null } }
}
