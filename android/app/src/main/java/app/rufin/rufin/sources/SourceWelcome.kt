package app.rufin.rufin.sources

import android.net.Uri
import android.provider.DocumentsContract
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import app.rufin.rufin.R
import app.rufin.rufin.app.RufinConnection
import app.rufin.rufin.connect.ConnectPage
import app.rufin.rufin.core.*
import app.rufin.rufin.player.KaraokeModeSwitch
import app.rufin.rufin.player.LoudnessNormalizationChoice
import app.rufin.rufin.player.PlayerConnection
import app.rufin.rufin.player.SimilarArtistsSwitch
import app.rufin.rufin.player.TransitionMode
import app.rufin.rufin.settings.ExternalLyricsSwitch
import app.rufin.rufin.settings.ExternalMetadataSwitch
import app.rufin.rufin.settings.IntegrationDetailPage
import app.rufin.rufin.settings.IntegrationPage
import app.rufin.rufin.settings.KeepRunningInBackground
import app.rufin.rufin.settings.LocalReduceMotion
import app.rufin.rufin.settings.PreferencesConnection
import app.rufin.rufin.settings.PrivateModeSwitch
import app.rufin.rufin.settings.ScrobblingAccountRows
import app.rufin.rufin.settings.SettingsGroup
import app.rufin.rufin.settings.SettingsPage
import app.rufin.rufin.settings.SettingsRow
import app.rufin.rufin.settings.SettingsSwitch
import app.rufin.rufin.settings.ThemeSettingsPage
import app.rufin.rufin.settings.UpdateCheckInterval
import app.rufin.rufin.settings.relayPage
import app.rufin.rufin.shared.rufin_core.SourceOperation
import app.rufin.rufin.ui.LocalPlayerBottomPadding
import app.rufin.rufin.ui.RufinIcon
import app.rufin.rufin.ui.SourceIcon
import kotlinx.coroutines.launch

/** What the first run's pages reach outside source setup. */
internal class SourceWelcome(val preferences: PreferencesConnection, val player: PlayerConnection,
    val importBackup: () -> Unit, val join: (String) -> Unit)

/** The source list: what was found first, then every kind. The first run opens on it with a greeting. */
@Composable
internal fun SourceChoices(model: RufinConnection, setup: SourceSetupConnection, welcome: SourceWelcome?, modifier: Modifier,
    chooseFolder: (Uri?) -> Unit, added: () -> Unit, skip: () -> Unit) {
    val next: @Composable () -> Unit = { RufinIcon("rufin-go-next-symbolic", null) }
    LazyColumn(modifier, contentPadding = PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp)) {
        // The greeting, centred as GTK's: the icon and title lead, Skip stays a quiet corner button.
        if (welcome != null) item { Box(Modifier.fillMaxWidth()) {
            Column(Modifier.fillMaxWidth().padding(top = 28.dp, bottom = 8.dp), horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Image(painterResource(R.drawable.app_icon), null, Modifier.padding(bottom = 4.dp).size(80.dp))
                Text(translate("Welcome to Rufin"), style = MaterialTheme.typography.headlineMedium, textAlign = TextAlign.Center)
                Text(translate("Choose where your music is. Add more places later in Settings."),
                    color = MaterialTheme.colorScheme.onSurfaceVariant, textAlign = TextAlign.Center)
            }
            TextButton(skip, Modifier.align(Alignment.TopEnd)) { Text(translate("Skip")) }
        } }
        item { SettingsGroup(translate("Found")) {
            // A folder grant lists every file, so lyrics and CUE sheets beside the music are read;
            // the media library holds audio only. The picker opens on the shared Music folder.
            SettingsRow(translate("Music on this phone"), summary = translate("Choose a local music folder"),
                leading = { RufinIcon("rufin-folder-music-symbolic", null) },
                trailing = { FilledTonalButton({
                    chooseFolder(DocumentsContract.buildDocumentUri("com.android.externalstorage.documents", "primary:Music"))
                }, enabled = !setup.working) { Text(translate("Add")) } })
            val found = setup.found
            if (found == null) SettingsRow(translate("Search this network"),
                summary = translate(if (welcome != null) "Servers and other Rufin devices" else "Jellyfin, Emby and Plex servers"),
                leading = { RufinIcon("rufin-network-server-symbolic", null) },
                trailing = { if (setup.finding) CircularProgressIndicator(Modifier.size(24.dp), strokeWidth = 2.dp) else next() },
                modifier = Modifier.clickable(enabled = !setup.finding, onClick = { setup.find(welcome != null) }))
            else {
                found.forEach { server ->
                    SettingsRow(server.name, summary = "${setup.providers.firstOrNull { it.first == server.kind }?.second ?: server.kind}, ${server.address}",
                        leading = { SourceIcon(server.kind, Modifier.size(24.dp)) }, trailing = next,
                        modifier = Modifier.clickable { setup.selectProvider(server.kind, server) })
                }
                if (welcome != null) model.connect.state?.devices.orEmpty().filter { !it.enrolled }.forEach { device ->
                    SettingsRow(device.name, summary = translate("Join to copy its sources and sign-ins"),
                        leading = { RufinIcon("rufin-phonelink-symbolic", null) }, trailing = next,
                        modifier = Modifier.clickable { welcome.join(device.id) })
                }
                SettingsRow(translate(if (setup.finding) "Searching..." else if (found.isEmpty()) "No Servers Found" else "Search Again"),
                    leading = { RufinIcon("rufin-view-refresh-symbolic", null) },
                    modifier = Modifier.clickable(enabled = !setup.finding, onClick = { setup.find(welcome != null) }))
            }
        } }
        item { SettingsGroup(translate("Add Manually")) {
            setup.providers.chunked(2).forEach { pair -> Row(horizontalArrangement = Arrangement.spacedBy(3.dp)) {
                pair.forEach { provider ->
                    SettingsRow(provider.second, leading = { SourceIcon(provider.first, Modifier.size(24.dp)) }, titleLines = 1,
                        modifier = Modifier.weight(1f).clickable { setup.selectProvider(provider.first) })
                }
            } }
        } }
        if (welcome != null) item { Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.Center) {
            TextButton(welcome.importBackup) { Text(translate("Import Backup")) }
        } }
    }
}

/** The first run's pages once a source is added: what to settle while the library loads, over its progress. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun SourceWizard(model: RufinConnection, welcome: SourceWelcome, close: () -> Unit) {
    val preferences = welcome.preferences
    val player = welcome.player
    val pager = rememberPagerState { 3 }
    val scope = rememberCoroutineScope()
    val reduceMotion = LocalReduceMotion.current
    fun show(page: Int) { scope.launch { if (reduceMotion) pager.scrollToPage(page) else pager.animateScrollToPage(page) } }
    var integration by rememberSaveable { mutableStateOf<IntegrationPage?>(null) }
    val accounts = preferences.accounts
    LaunchedEffect(preferences) { accounts.load() }
    val autoDj by remember(player) { derivedStateOf { player.playback?.controls?.autoDjEnabled == true } }
    // Back steps through the pages, and leaves from the first.
    BackHandler { if (pager.currentPage > 0) show(pager.currentPage - 1) else close() }
    Scaffold(bottomBar = {
        Surface {
            Column(Modifier.fillMaxWidth().navigationBarsPadding().padding(horizontal = 16.dp, vertical = 8.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp)) {
                LibraryProgress(model)
                Row(verticalAlignment = Alignment.CenterVertically) {
                    // The sides are as wide as each other, so the dots stay in the middle.
                    Box(Modifier.weight(1f)) {
                        if (pager.currentPage > 0) FilledTonalButton({ show(pager.currentPage - 1) }) { Text(translate("Back")) }
                    }
                    Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                        repeat(pager.pageCount) { page ->
                            Box(Modifier.size(6.dp).background(MaterialTheme.colorScheme.onSurface.copy(
                                alpha = if (page == pager.currentPage) 1f else .3f), CircleShape))
                        }
                    }
                    val last = pager.currentPage == pager.pageCount - 1
                    Box(Modifier.weight(1f).padding(start = 8.dp), contentAlignment = Alignment.CenterEnd) {
                        Button({ if (last) close() else show(pager.currentPage + 1) }) { Text(translate(if (last) "Start Listening" else "Next")) }
                    }
                }
            }
        }
    }) { padding ->
        CompositionLocalProvider(LocalPlayerBottomPadding provides 0.dp) {
            HorizontalPager(pager, Modifier.fillMaxSize().padding(padding).consumeWindowInsets(padding), verticalAlignment = Alignment.Top) { page ->
                when (page) {
                    0 -> SettingsPage {
                        SettingsGroup(translate("Online Lookups"),
                            translate("Rufin asks these services about the music you play. All of this can be changed later in Settings.")) {
                            ExternalMetadataSwitch(preferences, player, translate("Artist pictures, covers and tags from MusicBrainz, Last.fm and Wikidata"))
                            ExternalLyricsSwitch(player, translate("Lyrics from LRCLIB, NetEase, Genius and SimpMusic when a track has none"))
                            KaraokeModeSwitch(player, translate("Uses available karaoke lyrics or fetches them from the internet"))
                            UpdateCheckInterval(preferences, translate("New Rufin releases from GitHub"))
                            PrivateModeSwitch(preferences, translate("Turns off lookups, scrobbling, play reporting and update checks"))
                        }
                        SettingsGroup(translate("Scrobbling")) {
                            accounts.state?.let { state -> ScrobblingAccountRows(state) { integration = it } }
                        }
                    }
                    1 -> SettingsPage {
                        SettingsGroup(translate("Playback")) {
                            // A station plays alone, and a room's Auto DJ is its host's, as on the player.
                            SettingsSwitch(translate("Auto DJ"), autoDj, enabled = !player.live && model.room.room?.hosting != false) { player.autoDj() }
                            SimilarArtistsSwitch(preferences)
                            TransitionMode(player)
                            LoudnessNormalizationChoice(player)
                        }
                        Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                            Text(translate("Rufin Connect"), Modifier.padding(start = 12.dp), style = MaterialTheme.typography.titleSmall,
                                color = MaterialTheme.colorScheme.primary)
                            ConnectPage(model, preferences, { integration = relayPage() }, embedded = true, setup = true)
                        }
                        SettingsGroup { KeepRunningInBackground(preferences) }
                    }
                    else -> ThemeSettingsPage(preferences, translate("Theme"))
                }
            }
        }
    }
    // A scrobbling service's page, or the relay's, opens over the pages and returns to them.
    integration?.let { page ->
        Dialog(onDismissRequest = { integration = null },
            properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false)) {
            Scaffold(topBar = {
                TopAppBar(title = { Text(page.name) }, navigationIcon = {
                    IconButton({ integration = null }) { RufinIcon("rufin-go-previous-symbolic", translate("Back")) }
                })
            }) { padding ->
                Box(Modifier.fillMaxSize().padding(padding).consumeWindowInsets(padding)) {
                    CompositionLocalProvider(LocalPlayerBottomPadding provides 0.dp) {
                        IntegrationDetailPage(model, preferences, player, page, { integration = it }) { integration = null }
                    }
                }
            }
        }
    }
}

/** How far the library is: the sync's status over its bar, then that it is ready. */
@Composable
private fun LibraryProgress(model: RufinConnection) {
    val failed = model.sourceOperation as? SourceOperation.Failed
    if (failed != null) app.rufin.rufin.ui.ErrorNotice(failed.message)
    else if (model.sourceBusy) Column(Modifier.fillMaxWidth(), horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(6.dp)) {
        model.sourceStatus?.let {
            Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, textAlign = TextAlign.Center)
        }
        val progress = model.sourceProgress
        val total = progress?.total
        if (progress != null && total != null && total > 0UL) LinearProgressIndicator(progress = {
            (progress.completed.toFloat() / total.toFloat()).coerceIn(0f, 1f)
        }, modifier = Modifier.fillMaxWidth())
        else LinearProgressIndicator(Modifier.fillMaxWidth())
    } else Row(Modifier.fillMaxWidth(), Arrangement.spacedBy(8.dp, Alignment.CenterHorizontally), Alignment.CenterVertically) {
        RufinIcon("rufin-object-select-symbolic", null, Modifier.size(18.dp), MaterialTheme.colorScheme.primary)
        Text(translate("Your library is ready"), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}
