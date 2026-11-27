package app.rufin.rufin.more

import app.rufin.rufin.settings.CustomizationSettingsPage
import app.rufin.rufin.player.PlaybackPreferences
import app.rufin.rufin.settings.ThemeSettingsPage
import app.rufin.rufin.settings.StorageSettingsPage
import app.rufin.rufin.settings.OverviewSettingsPage
import app.rufin.rufin.settings.BackupSettingsPage
import app.rufin.rufin.activity.ListeningOverviewPage
import app.rufin.rufin.player.PlayerConnection
import app.rufin.rufin.player.PlayerSettingsContent
import app.rufin.rufin.player.PlayerSettingsPage
import androidx.compose.animation.AnimatedContent
import app.rufin.rufin.ui.pageMotion
import app.rufin.rufin.settings.PreferencesConnection
import app.rufin.rufin.app.RufinConnection
import app.rufin.rufin.browse.BrowseConnection
import app.rufin.rufin.controller.ControllerPage
import app.rufin.rufin.connect.ConnectPage
import app.rufin.rufin.sources.SourcesPage
import app.rufin.rufin.settings.AppSettingsPage
import app.rufin.rufin.settings.IntegrationDetailPage
import app.rufin.rufin.settings.IntegrationPage
import app.rufin.rufin.settings.IntegrationsPage
import app.rufin.rufin.settings.SettingsPage
import app.rufin.rufin.settings.SettingsGroup
import app.rufin.rufin.settings.SettingsRow
import app.rufin.rufin.settings.SettingsIcon
import app.rufin.rufin.settings.SettingsLeads
import app.rufin.rufin.settings.SettingsHighlight
import app.rufin.rufin.settings.SettingsSearchItem
import app.rufin.rufin.settings.SettingsSearchResults
import app.rufin.rufin.settings.SettingsTarget
import app.rufin.rufin.settings.LocalSettingsHighlight
import app.rufin.rufin.settings.settingsSearchItems
import kotlinx.coroutines.delay
import app.rufin.rufin.ui.RufinIcon
import app.rufin.rufin.shared.playback.PlaybackTransitionMode
import app.rufin.rufin.shared.playback.LoudnessNormalization
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.Alignment
import androidx.compose.ui.unit.dp
import app.rufin.rufin.settings.LocalReduceMotion
import app.rufin.rufin.downloads.DownloadsPage
import app.rufin.rufin.downloads.DownloadSettingsPage
import app.rufin.rufin.diagnostics.LogsPage
import app.rufin.rufin.releases.VersionHistoryPage

import androidx.compose.foundation.layout.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveableStateHolder
import androidx.compose.ui.Modifier
import app.rufin.rufin.core.translate

internal enum class MoreDestination(val title: String) {
    Root("Settings"), About("About Rufin"), Overview("Listening overview"), OverviewSettings("Listening overview"),
    Versions("Version History"), Logs("Logs"),
    General("General"), Customization("Customization"), Theme("Theme"), Integrations("Integrations"), Integration("Integrations"),
    Playback("Playback"), Equalizer("Equalizer"), Library("Library"), Downloads("Downloads"),
    Connect("Connect"), Controller("Controller"), Storage("Storage"), Privacy("Privacy & Security"),
    DownloadSettings("Downloads"),
    BackupSettings("Backup Settings");

    // The sidebar opens Settings and About; About lists the three pages about the app itself.
    val parent get() = when (this) {
        Root, About -> null
        Overview, Versions, Logs -> About
        Integration -> Integrations
        BackupSettings -> Privacy
        OverviewSettings -> General
        DownloadSettings -> Downloads
        else -> Root
    }
    val depth: Int get() = parent?.let { it.depth + 1 } ?: 0
    /** Which of the sidebar's two this page is under: About or Settings. */
    val section: MoreDestination get() = parent?.section ?: this
    /** The page Settings' root lists that this one is under, or this one. */
    val category: MoreDestination get() = parent?.takeIf { it != Root }?.category ?: this
    /** The icon of a page on Settings' root or on About. */
    val icon: String? get() = when (this) {
        Overview -> "rufin-activity-overview-symbolic"
        Versions -> "rufin-appointment-new-symbolic"
        Logs -> "rufin-rich-text-symbolic"
        General -> "rufin-preferences-system-symbolic"
        Customization -> "rufin-sliders-2-symbolic"
        Theme -> "rufin-preferences-desktop-appearance-symbolic"
        Integrations -> "rufin-network-workgroup-symbolic"
        Playback -> "rufin-music-queue-symbolic"
        Equalizer -> "rufin-sliders-vertical-symbolic"
        Library -> "rufin-library-symbolic"
        Downloads -> "rufin-download-symbolic"
        Connect -> "rufin-network-hotspot-symbolic"
        Controller -> "rufin-phonelink-symbolic"
        Storage -> "rufin-drive-harddisk-symbolic"
        Privacy -> "rufin-system-lock-screen-symbolic"
        else -> null
    }
}

@Composable
internal fun MoreContent(model: RufinConnection, preferences: PreferencesConnection, player: PlayerConnection, browse: BrowseConnection,
    destination: MoreDestination, navigate: (MoreDestination) -> Unit,
    /** The integration page Integration shows, and how one opens. */
    integration: IntegrationPage?, openIntegration: (IntegrationPage) -> Unit, back: () -> Unit,
    onAddSource: () -> Unit, onEditSource: (String) -> Unit, onOpen: () -> Unit, onOutput: () -> Unit,
    navigationDepth: Int,
    /** What Search on Settings' root looks for, while it is open there. */
    settingsQuery: String?,
    /** Opens the page a search result names; its row then shows itself. */
    openSearchResult: (SettingsSearchItem) -> Unit,
) {
    val more = model.more
    val highlight = remember { SettingsHighlight() }
    // A row the page no longer has, or one under a tab not shown, does not wait for a later visit.
    LaunchedEffect(highlight.target) {
        val target = highlight.target ?: return@LaunchedEffect
        delay(4000)
        if (highlight.target === target) highlight.target = null
    }
    // Read when Search opens, so it finds the sources and routes as they are then.
    val searchItems = remember(settingsQuery != null) {
        if (settingsQuery == null) emptyList() else settingsSearchItems(model, preferences, browse)
    }
    val reduceMotion = LocalReduceMotion.current
    val destinations = rememberSaveableStateHolder()
    LaunchedEffect(destination, more.bridge) {
        more.overviewVisible = destination == MoreDestination.Overview
        if (more.overviewVisible && more.bridge != null) more.prepareOverview()
    }
    DisposableEffect(more) { onDispose { more.overviewVisible = false } }
    AnimatedContent(destination to navigationDepth, Modifier.fillMaxSize(), transitionSpec = {
        pageMotion(targetState.second > initialState.second, reduceMotion)
    }, contentKey = { it.first }, label = "moreNavigation") { location ->
    val visible = location.first
    CompositionLocalProvider(LocalSettingsHighlight provides highlight) {
    destinations.SaveableStateProvider(visible.name) {
    when (visible) {
        MoreDestination.Root -> if (!settingsQuery.isNullOrBlank()) SettingsSearchResults(searchItems, settingsQuery) { item ->
            highlight.target = if (item.target != null || item.tab != null) SettingsTarget(item.target, item.tab) else null
            openSearchResult(item)
        } else SettingsPage {
            // Each page's row says what is set on it, where one line can.
            val language = preferences.state?.let { state ->
                state.languages.firstOrNull { it.id == state.language && it.id != "system" }?.title ?: translate("System default")
            }
            val start = preferences.applicationSettings?.let { settings -> browse.routes.firstOrNull { it.item.name == settings.startingPage.name }?.title }
            val playback = player.settings?.playback?.let { playback ->
                listOf(translate(if (playback.transitionMode == PlaybackTransitionMode.CROSSFADE) "Crossfade" else "Gapless"),
                    when (playback.loudnessNormalization) {
                        LoudnessNormalization.OFF -> null
                        LoudnessNormalization.REPLAY_GAIN -> "ReplayGain"
                        LoudnessNormalization.EBU_R128 -> "EBU R128"
                    })
            }
            SettingsGroup {
                SettingsCategory(MoreDestination.General, navigate, listOfNotNull(language, start))
                SettingsCategory(MoreDestination.Customization, navigate)
                SettingsCategory(MoreDestination.Theme, navigate)
            }
            SettingsGroup {
                SettingsCategory(MoreDestination.Integrations, navigate)
            }
            SettingsGroup {
                SettingsCategory(MoreDestination.Playback, navigate, playback.orEmpty().filterNotNull())
                SettingsCategory(MoreDestination.Equalizer, navigate)
            }
            SettingsGroup {
                SettingsCategory(MoreDestination.Library, navigate, model.sources?.sources.orEmpty().map { it.name }.filter { it.isNotBlank() }, ", ")
                SettingsCategory(MoreDestination.Downloads, navigate)
            }
            SettingsGroup {
                SettingsCategory(MoreDestination.Connect, navigate)
                SettingsCategory(MoreDestination.Controller, navigate)
            }
            SettingsGroup {
                SettingsCategory(MoreDestination.Storage, navigate)
                SettingsCategory(MoreDestination.Privacy, navigate)
            }
        }
        MoreDestination.Library -> SourcesPage(model, onAddSource, onEditSource)
        MoreDestination.General,
        MoreDestination.Privacy -> AppSettingsPage(visible, model, preferences, player, browse, navigate)
        MoreDestination.Playback -> PlaybackPreferences(player, preferences, model.sources?.sources.orEmpty(), onOutput)
        MoreDestination.Integrations -> IntegrationsPage(model, preferences, player, openIntegration)
        MoreDestination.Integration -> integration?.let { page -> key(page.id) {
            IntegrationDetailPage(model, preferences, player, page, openIntegration, back)
        } } ?: LaunchedEffect(Unit) { back() }
        MoreDestination.Downloads -> DownloadsPage(model, player) { navigate(MoreDestination.DownloadSettings) }
        MoreDestination.DownloadSettings -> DownloadSettingsPage(model, preferences)
        MoreDestination.BackupSettings -> BackupSettingsPage(model, preferences)
        MoreDestination.OverviewSettings -> OverviewSettingsPage(preferences)
        MoreDestination.Customization -> CustomizationSettingsPage(preferences, browse, player)
        MoreDestination.Theme -> ThemeSettingsPage(preferences)
        MoreDestination.Storage -> StorageSettingsPage(model, preferences)
        MoreDestination.Equalizer -> PlayerSettingsContent(player, PlayerSettingsPage.EQUALIZER)
        MoreDestination.Controller -> ControllerPage(model)
        MoreDestination.Connect -> ConnectPage(model)
        MoreDestination.Overview -> ListeningOverviewPage(model, more, player)
        MoreDestination.Versions -> VersionHistoryPage(more)
        MoreDestination.About -> AboutPage(navigate)
        MoreDestination.Logs -> LogsPage(model)
    }
    }
    }
    }
}

/** A page of Settings on its root: its icon, its name, and what is set there. */
@Composable
private fun SettingsCategory(page: MoreDestination, navigate: (MoreDestination) -> Unit, facts: List<String> = emptyList(),
    separator: String = " · ") {
    SettingsRow(translate(page.title), summary = facts.joinToString(separator).takeIf { it.isNotEmpty() },
        leading = { SettingsIcon(checkNotNull(page.icon)) }, trailing = { SettingsLeads() }, modifier = Modifier.clickable { navigate(page) })
}
