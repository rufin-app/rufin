package app.rufin.rufin.app

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.graphics.BlendMode
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.CompositingStrategy
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.tween
import androidx.compose.animation.expandVertically
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.shrinkVertically
import app.rufin.rufin.settings.LocalReduceMotion
import androidx.paging.compose.collectAsLazyPagingItems
import app.rufin.rufin.BuildConfig
import app.rufin.rufin.browse.AppSection
import app.rufin.rufin.browse.BrowseConnection
import app.rufin.rufin.browse.BrowseItem
import app.rufin.rufin.browse.BrowsePage
import app.rufin.rufin.browse.subtitle
import app.rufin.rufin.browse.title
import app.rufin.rufin.core.translate
import app.rufin.rufin.more.MoreDestination
import app.rufin.rufin.player.PlayerConnection
import app.rufin.rufin.player.PlayerIconButton
import app.rufin.rufin.shared.rufin_core.SourceSummary
import app.rufin.rufin.core.AndroidMusicFolderState
import kotlinx.coroutines.CancellationException
import app.rufin.rufin.ui.CoverShape
import app.rufin.rufin.ui.RufinIcon
import app.rufin.rufin.ui.SourceIcon

/** What the sidebar opens: a route, a saved entry, one of the app's own pages, or the source pages. */
internal class SidebarActions(
    val route: (String) -> Unit,
    val saved: (BrowseItem) -> Unit,
    val more: (MoreDestination) -> Unit,
    val addSource: () -> Unit,
    /** The general search, where no bottom bar or rail offers it. */
    val search: (() -> Unit)? = null,
)

/** The full sidebar: the picked source as the title, which opens the sources and its server libraries; the
 *  general search where no bottom bar offers it, Home and the routes, and the Saved entries; a docked player
 *  from edge to edge; and last Rufin with its version, which opens About, beside Settings. */
@Composable
internal fun Sidebar(model: RufinConnection, browse: BrowseConnection, player: PlayerConnection,
    saved: BrowsePage, actions: SidebarActions,
    /** The app's page that is open, which the last row marks. */
    open: MoreDestination?, modifier: Modifier = Modifier,
    /** The player that a short, wide screen docks under the list. */
    dock: (@Composable () -> Unit)? = null,
    /** Folds a docked sidebar to its compact rail, where the screen offers both. */
    collapse: (() -> Unit)? = null,
) {
    val sources = model.sources
    val selectedId = sources?.selectedSourceId
    var folders by remember(selectedId) { mutableStateOf<AndroidMusicFolderState?>(null) }
    LaunchedEffect(browse.library, sources, model.catalogChanges) {
        val library = browse.library ?: return@LaunchedEffect
        if (selectedId != model.sources?.selectedSourceId) return@LaunchedEffect
        try { folders = library.musicFolders() }
        catch (cancelled: CancellationException) { throw cancelled }
        catch (failure: Exception) { model.runAction { throw failure } }
    }
    val choices = folders?.choices.orEmpty().takeIf { list -> list.any { it.id != null } }.orEmpty()
    var switching by remember { mutableStateOf(false) }
    Column(modifier.fillMaxHeight().widthIn(max = 326.dp)) {
        SourceTitle(sources?.sources?.firstOrNull { it.id == selectedId },
            choices.firstOrNull { it.id == folders?.selectedId }?.title, switching, { switching = !switching }, collapse)
        SidebarList(browse, player, saved, actions, Modifier.weight(1f)) {
            SourceSwitcher(switching, sources?.sources.orEmpty(), selectedId, choices.map { it.title },
                choices.indexOfFirst { it.id == folders?.selectedId },
                pickSource = { switching = false; model.selectSource(it) },
                pickLibrary = { index -> switching = false; model.runAction { browse.library?.setMusicFolder(choices[index].id) } },
                manage = { actions.more(MoreDestination.Library) }, add = actions.addSource)
        }
        dock?.invoke()
        if (dock == null) HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
        // Only the bar under the sidebar: on its side a phone has the bar at the screen's far edge.
        Row(Modifier.fillMaxWidth().windowInsetsPadding(WindowInsets.navigationBars.only(WindowInsetsSides.Bottom))
            .padding(start = 8.dp, end = 4.dp, top = 4.dp, bottom = 4.dp),
            verticalAlignment = Alignment.CenterVertically) {
            NavigationDrawerItem(label = {
                Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    Text("Rufin", color = MaterialTheme.colorScheme.onSurface)
                    Text("v${BuildConfig.VERSION_NAME}", maxLines = 1, overflow = TextOverflow.Ellipsis, fontWeight = FontWeight.Normal)
                }
            }, selected = open?.section == MoreDestination.About, onClick = { actions.more(MoreDestination.About) },
                icon = { RufinIcon(AppIcon, null, Modifier.size(22.dp)) },
                modifier = Modifier.weight(1f).height(48.dp).semantics { contentDescription = translate("About Rufin") })
            CompositionLocalProvider(LocalContentColor provides MaterialTheme.colorScheme.onSurfaceVariant) {
                PlayerIconButton("rufin-preferences-system-symbolic", "Settings", { actions.more(MoreDestination.Root) },
                    selected = open?.section == MoreDestination.Root, iconSize = 24.dp)
            }
        }
    }
}

/** Rufin's own symbolic icon, which the build names for the covers it stands in for. */
private const val AppIcon = "rufin-cover-fallback-symbolic"

/** The picked source as the sidebar's title: its logo, its name with its server library under it, and an arrow
 *  that says the title opens. With no source the title is the app's name. A dot after the name says its server
 *  does not answer. */
@Composable
private fun SourceTitle(source: SourceSummary?, library: String?, open: Boolean, toggle: () -> Unit, collapse: (() -> Unit)?) {
    val scheme = MaterialTheme.colorScheme
    val turn by animateFloatAsState(if (open) 180f else 0f, tween(if (LocalReduceMotion.current) 0 else 180), label = "sourceTitle")
    Row(Modifier.fillMaxWidth().height(60.dp).padding(start = 4.dp, end = 4.dp), verticalAlignment = Alignment.CenterVertically) {
        Row(Modifier.weight(1f).clip(RoundedCornerShape(16.dp)).clickable(role = Role.Button, onClick = toggle)
            .padding(start = 16.dp, end = 8.dp, top = 6.dp, bottom = 6.dp), verticalAlignment = Alignment.CenterVertically) {
            if (source != null) SourceIcon(source.kind, Modifier.size(32.dp))
            else RufinIcon(AppIcon, null, Modifier.size(32.dp), scheme.primary)
            Column(Modifier.weight(1f, fill = false).padding(start = 12.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(source?.name ?: "Rufin", Modifier.weight(1f, fill = false), maxLines = 1, overflow = TextOverflow.Ellipsis,
                        style = MaterialTheme.typography.titleLarge, fontWeight = FontWeight.SemiBold)
                    if (source?.unreachable == true) {
                        val label = translate("Server is unreachable")
                        Box(Modifier.size(8.dp).background(scheme.error, CircleShape).semantics { contentDescription = label })
                    }
                }
                library?.let {
                    Text(it, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodySmall, color = scheme.onSurfaceVariant)
                }
            }
            RufinIcon("rufin-pan-down-symbolic", null, Modifier.padding(start = 4.dp).size(16.dp).graphicsLayer { rotationZ = turn }, scheme.onSurfaceVariant)
        }
        collapse?.let { PlayerIconButton("rufin-sidebar-show-symbolic", "Collapse sidebar", it) }
    }
}

/** What the title opens, over the routes: the sources, the picked one's server libraries under it, then Manage
 *  and Add Source. One group of segments, as Settings' rows. */
@Composable
private fun SourceSwitcher(open: Boolean, sources: List<SourceSummary>, selectedId: String?, libraries: List<String>, library: Int,
    pickSource: (String) -> Unit, pickLibrary: (Int) -> Unit, manage: () -> Unit, add: () -> Unit) {
    val scheme = MaterialTheme.colorScheme
    val duration = if (LocalReduceMotion.current) 0 else 180
    AnimatedVisibility(open, enter = expandVertically(tween(duration)) + fadeIn(tween(duration)),
        exit = shrinkVertically(tween(duration)) + fadeOut(tween(duration))) {
        Column(Modifier.fillMaxWidth().padding(bottom = 10.dp).clip(RoundedCornerShape(18.dp)), verticalArrangement = Arrangement.spacedBy(3.dp)) {
            @Composable
            fun Segment(selected: Boolean, modifier: Modifier, content: @Composable RowScope.() -> Unit) {
                Row(modifier.clip(RoundedCornerShape(5.dp)).background(if (selected) scheme.secondaryContainer else scheme.surfaceContainer)
                    .height(46.dp).padding(horizontal = 14.dp), verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(12.dp), content = content)
            }
            val label = MaterialTheme.typography.bodyMedium.copy(fontWeight = FontWeight.Medium)
            sources.forEach { source ->
                val selected = source.id == selectedId
                Segment(selected, Modifier.fillMaxWidth().selectable(selected, role = Role.RadioButton) { if (!selected) pickSource(source.id) }) {
                    SourceIcon(source.kind, Modifier.size(22.dp))
                    Text(source.name, Modifier.weight(1f), maxLines = 1, overflow = TextOverflow.Ellipsis, style = label)
                    if (selected) RufinIcon("rufin-object-select-symbolic", null, Modifier.size(18.dp), scheme.primary)
                }
                if (selected) libraries.forEachIndexed { index, title ->
                    val picked = index == library
                    Segment(false, Modifier.fillMaxWidth().selectable(picked, role = Role.RadioButton) { if (!picked) pickLibrary(index) }) {
                        Text(title, Modifier.weight(1f).padding(start = 34.dp), maxLines = 1, overflow = TextOverflow.Ellipsis, style = label)
                        if (picked) RufinIcon("rufin-object-select-symbolic", null, Modifier.size(18.dp), scheme.primary)
                    }
                }
            }
            Row(horizontalArrangement = Arrangement.spacedBy(3.dp)) {
                listOf(Triple("rufin-document-edit-symbolic", translate("Manage"), manage),
                    Triple("rufin-list-add-symbolic", translate("Add Source"), add)).forEach { (icon, title, action) ->
                    Segment(false, Modifier.weight(1f).clickable(role = Role.Button, onClick = action)) {
                        Row(Modifier.weight(1f), horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.CenterHorizontally),
                            verticalAlignment = Alignment.CenterVertically) {
                            RufinIcon(icon, null, Modifier.size(16.dp), scheme.onSurfaceVariant)
                            Text(title, maxLines = 1, overflow = TextOverflow.Ellipsis, style = label)
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun SidebarList(browse: BrowseConnection, player: PlayerConnection, saved: BrowsePage, actions: SidebarActions, modifier: Modifier,
    /** What the title opened, over the list. */
    switcher: @Composable () -> Unit) {
    val rows = saved.pages.collectAsLazyPagingItems()
    LazyColumn(modifier.fillMaxWidth(), contentPadding = PaddingValues(start = 8.dp, end = 8.dp, top = 4.dp, bottom = 16.dp)) {
        item { switcher() }
        actions.search?.let { search -> item {
            Surface(search, Modifier.fillMaxWidth().padding(bottom = 6.dp).height(44.dp), shape = RoundedCornerShape(22.dp),
                color = if (browse.section == AppSection.Search) MaterialTheme.colorScheme.secondaryContainer
                    else MaterialTheme.colorScheme.surfaceContainerHighest) {
                Row(Modifier.padding(horizontal = 14.dp), verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    RufinIcon("rufin-search-symbolic", null, Modifier.size(20.dp), MaterialTheme.colorScheme.onSurfaceVariant)
                    Text(translate("Search"), style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
        } }
        items(sidebarRoutes(browse), key = { it.id }) { route ->
            val selected = browse.section == AppSection.Browse && browse.rootId == route.id
            NavigationDrawerItem(label = { Text(route.title, maxLines = 1, overflow = TextOverflow.Ellipsis) }, selected = selected,
                onClick = { actions.route(route.id) },
                icon = { RufinIcon(if (selected) route.selectedIconName else route.iconName, null, Modifier.size(22.dp)) },
                modifier = Modifier.height(48.dp))
        }
        if (rows.itemCount > 0) item {
            Text(translate("Saved").uppercase(), Modifier.padding(start = 16.dp, top = 18.dp, bottom = 6.dp),
                style = MaterialTheme.typography.labelMedium, fontWeight = FontWeight.Bold,
                color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        items(rows.itemCount) { index ->
            rows[index]?.let { item -> SavedRow(item, player) { actions.saved(item) } }
        }
    }
}

/** Home and the configured routes, as the sidebar and the rail list them; Search has its tab. */
internal fun sidebarRoutes(browse: BrowseConnection) = browse.configuredRoutes.filter { it.id != "Search" }

@Composable
private fun SavedRow(item: BrowseItem, player: PlayerConnection, open: () -> Unit) {
    Row(Modifier.fillMaxWidth().height(52.dp).clip(RoundedCornerShape(12.dp)).clickable(onClick = open).padding(horizontal = 12.dp),
        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        app.rufin.rufin.browse.PinCover(item, player, 36.dp)
        Column(Modifier.weight(1f)) {
            Text(item.row.title, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodyMedium,
                fontWeight = FontWeight.Medium)
            CompositionLocalProvider(LocalContentColor provides MaterialTheme.colorScheme.onSurfaceVariant) {
                app.rufin.rufin.browse.PinFacts(item, player.model)
            }
        }
    }
}

/** The compact icon sidebar of foldable widths: ☰ opens the full one; then Search, Home and the routes, each
 *  with its name, and Saved with its entries' covers under it; About and Settings are pinned at its bottom. */
@Composable
internal fun SidebarRail(browse: BrowseConnection, player: PlayerConnection, saved: BrowsePage, expand: () -> Unit,
    route: (String) -> Unit, search: () -> Unit, onSaved: () -> Unit, openSaved: (BrowseItem) -> Unit,
    /** The app's page that is open, and how one opens. */
    open: MoreDestination?, more: (MoreDestination) -> Unit) {
    val rows = saved.pages.collectAsLazyPagingItems()
    Surface(Modifier.fillMaxHeight().width(84.dp), color = MaterialTheme.colorScheme.surfaceContainerLow) {
        Column(Modifier.statusBarsPadding().padding(top = 4.dp), horizontalAlignment = Alignment.CenterHorizontally) {
            PlayerIconButton("rufin-open-menu-symbolic", "Expand sidebar", expand)
            // A long list scrolls, and fades out at the bottom edge.
            LazyColumn(Modifier.weight(1f).fillMaxWidth()
                .graphicsLayer { compositingStrategy = CompositingStrategy.Offscreen }
                .drawWithContent {
                    drawContent()
                    drawRect(Brush.verticalGradient(0f to Color.Black, .88f to Color.Black, 1f to Color.Transparent), blendMode = BlendMode.DstIn)
                }, horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(10.dp),
                contentPadding = PaddingValues(top = 6.dp, bottom = 40.dp)) {
                item { RailItem("rufin-search-symbolic", translate("Search"), browse.section == AppSection.Search, search) }
                items(sidebarRoutes(browse), key = { it.id }) { item ->
                    val selected = browse.section == AppSection.Browse && browse.rootId == item.id
                    RailItem(if (selected) item.selectedIconName else item.iconName, item.title, selected) { route(item.id) }
                }
                item { HorizontalDivider(Modifier.width(40.dp).padding(vertical = 2.dp)) }
                item { RailItem("rufin-bookmark-symbolic", translate("Saved"), browse.section == AppSection.Pins, onSaved) }
                items(rows.itemCount) { index ->
                    rows[index]?.let { item ->
                        Box(Modifier.clip(CoverShape).clickable(onClickLabel = item.row.title) { openSaved(item) }
                            .semantics { contentDescription = item.row.title }) {
                            app.rufin.rufin.browse.PinCover(item, player, 48.dp)
                        }
                    }
                }
            }
            Column(Modifier.navigationBarsPadding().padding(bottom = 8.dp), horizontalAlignment = Alignment.CenterHorizontally) {
                CompositionLocalProvider(LocalContentColor provides MaterialTheme.colorScheme.onSurfaceVariant) {
                    PlayerIconButton(AppIcon, "About Rufin", { more(MoreDestination.About) }, selected = open?.section == MoreDestination.About)
                    PlayerIconButton("rufin-preferences-system-symbolic", "Settings", { more(MoreDestination.Root) },
                        selected = open?.section == MoreDestination.Root)
                }
            }
        }
    }
}

@Composable
private fun RailItem(icon: String, label: String, selected: Boolean, onClick: () -> Unit) {
    Column(Modifier.width(84.dp).selectable(selected, role = Role.Tab, onClick = onClick), horizontalAlignment = Alignment.CenterHorizontally) {
        // The picked one takes the accent, as the desktop's compact sidebar does; no bubble.
        Box(Modifier.size(52.dp, 30.dp), contentAlignment = Alignment.Center) {
            RufinIcon(icon, null, Modifier.size(22.dp),
                if (selected) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant)
        }
        Text(label, Modifier.padding(start = 4.dp, end = 4.dp, top = 3.dp), maxLines = 1, overflow = TextOverflow.Ellipsis,
            style = MaterialTheme.typography.labelSmall, fontWeight = if (selected) FontWeight.Bold else FontWeight.Medium,
            color = if (selected) MaterialTheme.colorScheme.onSurface else MaterialTheme.colorScheme.onSurfaceVariant)
    }
}
