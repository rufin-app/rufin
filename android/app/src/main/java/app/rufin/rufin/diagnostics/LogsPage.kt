package app.rufin.rufin.diagnostics

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import app.rufin.rufin.app.RufinConnection
import app.rufin.rufin.core.translate
import app.rufin.rufin.settings.SettingsGroup
import app.rufin.rufin.settings.SettingsRow
import app.rufin.rufin.shared.rufin_core.DiagnosticEntry
import app.rufin.rufin.shared.rufin_core.DiagnosticLevel
import app.rufin.rufin.ui.ChoiceChip
import app.rufin.rufin.ui.LocalPlayerBottomPadding
import app.rufin.rufin.ui.RufinIcon
import kotlinx.coroutines.*
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/** The app's log as entries: when, from where, the message, and its values under it. Warnings and errors are
 *  named and tinted, and their chips keep only them. The bar copies and saves the log as text. */
@Composable
internal fun LogsPage(model: RufinConnection) {
    val state = rememberLazyListState()
    var entries by remember { mutableStateOf<List<DiagnosticEntry>>(emptyList()) }
    var warnings by rememberSaveable { mutableStateOf(false) }
    var errors by rememberSaveable { mutableStateOf(false) }
    val shows by rememberUpdatedState { entry: DiagnosticEntry ->
        (!warnings && !errors) || (warnings && entry.level == DiagnosticLevel.WARNING) || (errors && entry.level == DiagnosticLevel.ERROR)
    }
    // A short screen has no room to keep the switch and the chips over the log: they scroll with it.
    val pinned = LocalConfiguration.current.screenHeightDp >= 480
    val lead = if (pinned) 0 else 1
    LaunchedEffect(model.service, model.ready) {
        val runtime = model.service?.runtime?.value?.getOrNull() ?: return@LaunchedEffect
        entries = emptyList()
        var cursor = 0uL
        while (isActive) {
            val update = withContext(Dispatchers.IO) { runtime.diagnosticUpdates(cursor) }
            if (update.end != cursor) {
                val tail = state.layoutInfo.totalItemsCount - 1
                val follow = tail <= lead || state.layoutInfo.visibleItemsInfo.lastOrNull()?.index == tail
                val kept = entries
                entries = withContext(Dispatchers.Default) { kept.dropWhile { it.position < update.start } + update.entries }
                cursor = update.end
                val count = entries.count(shows)
                if (follow && count > 0) state.scrollToItem(count - 1 + lead)
            }
            delay(250)
        }
    }
    val shown = remember(entries, warnings, errors) { entries.filter(shows) }
    val clock = remember { SimpleDateFormat("HH:mm:ss", Locale.ROOT) }
    val head: @Composable () -> Unit = {
        Column(Modifier.padding(bottom = 12.dp)) {
            SettingsGroup {
                SettingsRow(translate("Debug logging"), trailing = { Switch(model.debugLogging, model::changeDebugLogging, enabled = model.ready) })
            }
            Text(translate("Logs have secrets and absolute folder paths redacted, but you may still want to review the logs before sharing them"),
                Modifier.padding(start = 12.dp, end = 12.dp, top = 8.dp, bottom = 6.dp), style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant)
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                listOf(Triple(translate("Warnings"), DiagnosticLevel.WARNING, warnings),
                    Triple(translate("Errors"), DiagnosticLevel.ERROR, errors)).forEach { (title, level, picked) ->
                    val count = entries.count { it.level == level }
                    ChoiceChip(picked, { if (level == DiagnosticLevel.WARNING) warnings = !warnings else errors = !errors },
                        { Text(if (count > 0) "$title $count" else title) },
                        leadingIcon = if (picked) { { RufinIcon("rufin-object-select-symbolic", null, Modifier.size(18.dp)) } } else null)
                }
            }
        }
    }
    Column(Modifier.fillMaxSize().padding(start = 16.dp, end = 16.dp, top = 8.dp)) {
        if (pinned) head()
        Box(Modifier.weight(1f)) {
            LazyColumn(Modifier.fillMaxSize(), state = state, contentPadding = PaddingValues(bottom = 16.dp + LocalPlayerBottomPadding.current)) {
                if (!pinned) item { head() }
                itemsIndexed(shown, key = { _, entry -> entry.position.toLong() }) { index, entry ->
                    val corner = { edge: Boolean -> if (edge) 18.dp else 0.dp }
                    Column(Modifier.fillMaxWidth().clip(RoundedCornerShape(topStart = corner(index == 0), topEnd = corner(index == 0),
                        bottomStart = corner(index == shown.lastIndex), bottomEnd = corner(index == shown.lastIndex)))
                        .background(MaterialTheme.colorScheme.surfaceContainer)) {
                        if (index > 0) HorizontalDivider(color = MaterialTheme.colorScheme.onSurface.copy(alpha = .09f))
                        LogEntry(entry, clock)
                    }
                }
            }
        }
    }
}

@Composable
private fun LogEntry(entry: DiagnosticEntry, clock: SimpleDateFormat) {
    val scheme = MaterialTheme.colorScheme
    val dark = scheme.surface.luminance() < .5f
    // The warning and error text colours of the desktop's Adwaita.
    val (name, color) = when (entry.level) {
        DiagnosticLevel.ERROR -> "ERROR" to if (dark) Color(0xFFFF938C) else Color(0xFFC30000)
        DiagnosticLevel.WARNING -> "WARN" to if (dark) Color(0xFFFFC252) else Color(0xFF905400)
        DiagnosticLevel.DEBUG -> "DEBUG" to scheme.onSurfaceVariant
        DiagnosticLevel.TRACE -> "TRACE" to scheme.onSurfaceVariant
        DiagnosticLevel.INFO, null -> null to scheme.onSurfaceVariant
    }
    val tinted = entry.level == DiagnosticLevel.ERROR || entry.level == DiagnosticLevel.WARNING
    val quiet = entry.level == DiagnosticLevel.DEBUG || entry.level == DiagnosticLevel.TRACE
    // A row shows the start of its entry, as the desktop's does; a tap opens it to all of it.
    var open by rememberSaveable(entry.position) { mutableStateOf(false) }
    Row(Modifier.fillMaxWidth().background(if (tinted) color.copy(alpha = .12f) else Color.Transparent).clickable { open = !open }
        .padding(start = 12.dp, end = 14.dp, top = 9.dp, bottom = 9.dp), horizontalArrangement = Arrangement.spacedBy(10.dp)) {
        Column(Modifier.width(54.dp).padding(top = 2.dp)) {
            if (entry.level != null) Text(clock.format(Date(entry.timeMillis.toLong())), maxLines = 1, softWrap = false,
                style = MaterialTheme.typography.labelSmall, fontFamily = FontFamily.Monospace, color = scheme.onSurfaceVariant)
            name?.let { Text(it, style = MaterialTheme.typography.labelSmall, fontFamily = FontFamily.Monospace, fontWeight = FontWeight.Bold, color = color) }
        }
        Column(Modifier.weight(1f)) {
            if (entry.level == null) Text(entry.message, maxLines = if (open) Int.MAX_VALUE else 6, overflow = TextOverflow.Ellipsis,
                style = MaterialTheme.typography.bodySmall, fontFamily = FontFamily.Monospace)
            else {
                Text(entry.origin, style = MaterialTheme.typography.labelSmall, color = scheme.onSurfaceVariant)
                if (entry.message.isNotEmpty()) Text(entry.message, maxLines = if (open) Int.MAX_VALUE else 6, overflow = TextOverflow.Ellipsis,
                    style = MaterialTheme.typography.bodyMedium, color = if (quiet) scheme.onSurfaceVariant else scheme.onSurface)
                if (entry.fields.isNotEmpty()) Text(entry.fields, Modifier.padding(top = 2.dp), maxLines = if (open) Int.MAX_VALUE else 3,
                    overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodySmall, fontFamily = FontFamily.Monospace,
                    color = scheme.onSurfaceVariant)
            }
        }
    }
}
