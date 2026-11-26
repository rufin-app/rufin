package app.rufin.rufin.player

import android.graphics.Typeface
import android.os.SystemClock
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.interaction.collectIsDraggedAsState
import androidx.compose.foundation.gestures.animateScrollBy
import androidx.compose.foundation.gestures.scrollBy
import androidx.compose.foundation.gestures.stopScroll
import androidx.compose.animation.core.tween
import androidx.compose.animation.animateColorAsState
import androidx.compose.foundation.background
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.gestures.detectHorizontalDragGestures
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.BlendMode
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.CompositingStrategy
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.ui.draw.clip
import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.slideOutVertically
import androidx.compose.animation.togetherWith
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.saveable.listSaver
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Shadow
import androidx.compose.ui.graphics.lerp
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.graphics.drawscope.clipPath
import androidx.compose.ui.text.TextLayoutResult
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.drawText
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import app.rufin.rufin.ui.RufinIcon
import app.rufin.rufin.ui.ToolButton
import app.rufin.rufin.core.*
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.withContext
import app.rufin.rufin.shared.lyrics.UserPreferences
import app.rufin.rufin.shared.lyrics.LyricsAgentRole
import app.rufin.rufin.shared.lyrics.externalLyricsProviderTitle
import app.rufin.rufin.shared.lyrics.lyricsOffsetForLineAt
import app.rufin.rufin.shared.lyrics.shiftLrcTextTimestamps
import app.rufin.rufin.shared.playback.CurrentMediaId
import app.rufin.rufin.shared.playback.TransportStatus
import kotlinx.coroutines.CancellationException
import app.rufin.rufin.settings.LocalReduceMotion
import app.rufin.rufin.settings.settingsTextFieldColors
import kotlin.math.ceil

@Composable
/** `selection` is the lines picked to share while their selection is open, else null. */
internal fun LyricsPanel(player: PlayerConnection, state: LazyListState, selection: List<Int>? = null, onSelect: (List<Int>) -> Unit = {},
    /** The view's tools: missing lyrics then offer their search, and the sync mode takes the taps on lines. */
    tools: LyricsTools? = null) {
    LaunchedEffect(player.currentId, player.bridge, player.lyrics?.mediaId) { player.requestLyrics() }
    val current = player.currentLyrics
    val document = current?.document
    val preferences = player.settings?.lyrics
    Box(Modifier.fillMaxSize()) {
        when {
            current?.loading == true -> CircularProgressIndicator(Modifier.align(Alignment.Center))
            current?.instrumental == true -> Column(Modifier.align(Alignment.Center), horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(20.dp)) {
                RufinIcon("rufin-audio-x-generic-symbolic", null, Modifier.size(36.dp), LocalContentColor.current.copy(alpha = .55f))
                Text(translate("Instrumental"))
            }
            document == null -> Column(Modifier.align(Alignment.Center), horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(16.dp)) {
                Text(translate("No lyrics available."))
                if (tools != null && player.lyricsToken != null) Button({ tools.search = true },
                    enabled = player.settings?.application?.privateMode != true) {
                    RufinIcon("rufin-lyrics-search-symbolic", null, Modifier.size(18.dp))
                    Spacer(Modifier.width(8.dp))
                    Text(translate("Search lyrics"))
                }
            }
            else -> LyricsDocumentView(player, current, document, preferences, state, selection, onSelect, tools)
        }
    }
}

/** What the lyrics' tools have open: the search or the editor sheet, and the sync mode. */
internal class LyricsTools {
    var search by mutableStateOf(false)
    var edit by mutableStateOf(false)
    var offsetEntry by mutableStateOf(false)
    /** The offset the sync mode opened with, which Cancel puts back; null while the mode is closed. */
    var syncStart by mutableStateOf<Long?>(null)
    /** Whether the offset came from a picked line. */
    var picked by mutableStateOf(false)
    /** Save Lyrics, which [LyricsSheets] owns for the search sheet and the sync mode. */
    var save: () -> Unit = {}
    val syncing get() = syncStart != null
    fun sync(player: PlayerConnection) { picked = false; syncStart = player.lyricsOffset }
    fun cancel(player: PlayerConnection) { syncStart?.let { player.lyricsOffset = it }; syncStart = null }
    fun done() { syncStart = null }
}

/** The lyrics view's two tools: Search, and the offset where the lines are timed. "No lyrics available."
 *  carries its own Search. */
@Composable
internal fun LyricsToolButtons(player: PlayerConnection, tools: LyricsTools) {
    val current = player.currentLyrics
    val document = current?.document
    if (player.lyricsToken != null && (document != null || current?.loading == true || current?.instrumental == true))
        ToolButton("rufin-lyrics-search-symbolic", "Search lyrics", enabled = player.settings?.application?.privateMode != true) { tools.search = true }
    if (document?.lines?.any { it.startMillis != null } == true)
        ToolButton("rufin-preferences-system-time-symbolic", "Lyrics offset (ms)") { tools.sync(player) }
}

/** The sheets the lyrics' tools open, and Save Lyrics, which the search sheet and the sync mode share. */
@Composable
internal fun LyricsSheets(player: PlayerConnection, tools: LyricsTools) {
    val token = player.lyricsToken
    var exportToken by rememberSaveable(stateSaver = listSaver<CurrentMediaId?, String>(
        save = { media -> media?.let { listOf(it.mediaUri, it.run?.toString().orEmpty()) }.orEmpty() },
        restore = { values -> values.takeIf { it.size == 2 }?.let { CurrentMediaId(it[1].toULongOrNull(), it[0]) } },
    )) { mutableStateOf<CurrentMediaId?>(null) }
    var exportOffset by rememberSaveable { mutableLongStateOf(0L) }
    val export = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument("text/plain")) { uri ->
        if (uri != null) exportToken?.let { player.exportLyrics(uri, it, exportOffset) }
        exportToken = null
    }
    SideEffect {
        tools.save = {
            token?.let { captured ->
                player.saveLyrics(captured) {
                    exportToken = captured
                    exportOffset = player.lyricsOffset
                    export.launch("${player.currentItem?.metadata?.title.orEmpty()}.lrc")
                }
            }
        }
    }
    if (tools.search && token != null) LyricsSearchSheet(player, token, onEdit = { tools.search = false; tools.edit = true },
        onSave = { tools.search = false; tools.save() }) { tools.search = false }
    if (tools.edit && token != null) LyricsEditSheet(player, token) { tools.edit = false }
    if (tools.offsetEntry) LyricsOffsetDialog(player) { tools.offsetEntry = false }
}

/** The sync mode's bar: Cancel, which puts the offset back, the mode's title, and Done. */
@Composable
internal fun LyricsSyncBar(player: PlayerConnection, tools: LyricsTools, modifier: Modifier = Modifier) {
    Row(modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
        PlayerIconButton("rufin-window-close-symbolic", "Cancel", { tools.cancel(player) })
        Text(translate("Lyrics offset (ms)"), Modifier.weight(1f), textAlign = TextAlign.Center, maxLines = 1,
            style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.Bold)
        PlayerIconButton("rufin-object-select-symbolic", "Done", tools::done)
    }
}

/** The sync mode's controls under the lines: the offset on a ruler, Reset and Save Lyrics. A positive offset
 *  shows the lines earlier, so Earlier is the ruler's rising end. */
@Composable
internal fun LyricsSyncDock(player: PlayerConnection, tools: LyricsTools, needle: Color, modifier: Modifier = Modifier) {
    val offset = player.lyricsOffset
    // Saved or replaced lyrics start from their own zero: Cancel then has nothing older to put back.
    val snapshot = player.currentLyrics?.snapshot
    LaunchedEffect(snapshot) { if (tools.syncing) tools.syncStart = player.lyricsOffset }
    Surface(modifier.fillMaxWidth(), shape = RoundedCornerShape(18.dp), color = MaterialTheme.colorScheme.surfaceContainerHigh) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(translate("Later"), style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                Column(Modifier.weight(1f), horizontalAlignment = Alignment.CenterHorizontally) {
                    Text(offsetText(offset), Modifier.clip(MaterialTheme.shapes.small).clickable(onClickLabel = translate("Lyrics offset (ms)")) { tools.offsetEntry = true }
                        .padding(horizontal = 10.dp), style = MaterialTheme.typography.titleLarge.copy(fontFeatureSettings = "tnum"), fontWeight = FontWeight.Bold)
                    Text(translate(if (tools.picked) "Set from the line you picked" else "Drag the ruler, or pick a line as it starts"),
                        style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant, textAlign = TextAlign.Center)
                }
                Text(translate("Earlier"), style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            OffsetRuler(offset, needle) { tools.picked = false; player.lyricsOffset = it }
            Row(verticalAlignment = Alignment.CenterVertically) {
                val quiet = ButtonDefaults.filledTonalButtonColors(containerColor = MaterialTheme.colorScheme.onSurface.copy(alpha = .1f),
                    contentColor = MaterialTheme.colorScheme.onSurface)
                FilledTonalButton({ tools.picked = false; player.lyricsOffset = 0L }, enabled = offset != 0L, colors = quiet,
                    contentPadding = PaddingValues(horizontal = 14.dp)) {
                    RufinIcon("rufin-view-refresh-symbolic", null, Modifier.size(18.dp))
                    Spacer(Modifier.width(8.dp))
                    Text(translate("Reset"))
                }
                Spacer(Modifier.weight(1f))
                Button({ tools.save() }, contentPadding = PaddingValues(horizontal = 14.dp)) {
                    RufinIcon("rufin-download-symbolic", null, Modifier.size(18.dp))
                    Spacer(Modifier.width(8.dp))
                    Text(translate("Save Lyrics"))
                }
            }
        }
    }
}

private fun offsetText(millis: Long) = (if (millis > 0) "+" else "") + millis + " ms"

/** A tape of milliseconds under a fixed needle: a tick every 50 ms, a labelled one every 250, moved 10 ms at a time. */
@Composable
private fun OffsetRuler(value: Long, needle: Color, onChange: (Long) -> Unit) {
    val haptic = LocalHapticFeedback.current
    val latest by rememberUpdatedState(value)
    val change by rememberUpdatedState(onChange)
    val ticks = MaterialTheme.colorScheme.onSurface
    val labels = MaterialTheme.typography.labelSmall.copy(color = MaterialTheme.colorScheme.onSurfaceVariant, fontFeatureSettings = "tnum")
    val measurer = rememberTextMeasurer()
    val title = translate("Lyrics offset (ms)")
    Canvas(Modifier.fillMaxWidth().height(44.dp).semantics { contentDescription = title; stateDescription = offsetText(value) }
        .graphicsLayer { compositingStrategy = CompositingStrategy.Offscreen }
        .pointerInput(Unit) {
            // Five milliseconds to the dp; the tape follows the finger, so a drag left raises the offset.
            val scale = 1.dp.toPx() / 5f
            var carried = 0f
            detectHorizontalDragGestures(onDragStart = { carried = 0f }) { input, amount ->
                input.consume()
                carried -= amount / scale
                val step = (carried / 10f).toLong() * 10L
                if (step != 0L) {
                    carried -= step
                    val next = latest + step
                    if (Math.floorDiv(next, 50L) != Math.floorDiv(latest, 50L)) haptic.performHapticFeedback(HapticFeedbackType.SegmentFrequentTick)
                    change(next)
                }
            }
        }) {
        val scale = 1.dp.toPx() / 5f
        val centre = size.width / 2
        val reach = (centre / scale).toLong() + 50L
        var mark = Math.floorDiv(value - reach, 50L) * 50L
        while (mark <= value + reach) {
            val x = centre + (mark - value) * scale
            val major = Math.floorMod(mark, 250L) == 0L
            drawRoundRect(ticks.copy(alpha = if (major) 1f else .55f), Offset(x - (if (major) 1.dp else .5.dp).toPx(), 0f),
                Size((if (major) 2.dp else 1.dp).toPx(), (if (major) 18.dp else 9.dp).toPx()), CornerRadius(1.dp.toPx()))
            if (major) {
                val text = measurer.measure((if (mark > 0) "+" else "") + mark, labels)
                drawText(text, topLeft = Offset(x - text.size.width / 2f, 24.dp.toPx()))
            }
            mark += 50L
        }
        // The tape fades out towards its ends.
        drawRect(Brush.horizontalGradient(0f to Color.Transparent, .16f to Color.Black, .84f to Color.Black, 1f to Color.Transparent),
            blendMode = BlendMode.DstIn)
        drawRoundRect(needle, Offset(centre - 2.dp.toPx(), 0f), Size(4.dp.toPx(), 22.dp.toPx()), CornerRadius(2.dp.toPx()))
    }
}

@Composable
internal fun CurrentLyricsPreview(player: PlayerConnection) {
    LaunchedEffect(player.currentId, player.bridge, player.lyrics?.mediaId) { player.requestLyrics() }
    val current = player.currentLyrics
    val document = current?.document
    val snapshot = current?.snapshot
    val preferences = player.settings?.lyrics
    val karaoke = preferences?.karaokeMode == true
    val reduceMotion = LocalReduceMotion.current
    val frame by rememberLyricsFrame(player, snapshot, karaoke)
    val active = frame?.activeLine?.toInt()
    val highlights = frame?.highlights.orEmpty()
    val normal = MaterialTheme.colorScheme.onSurface
    val selected = preferences?.lyricsHighlightColor?.let { playerLyricsColor(it) }
        ?: MaterialTheme.colorScheme.primary
    val shadow = Shadow(MaterialTheme.colorScheme.background, Offset.Zero, 4f)
    val fontSize = MaterialTheme.typography.bodyLarge.fontSize.value.toInt()
    AnimatedContent(active, Modifier.fillMaxWidth(), transitionSpec = {
        (slideInVertically(tween(if (reduceMotion) 0 else 180)) { it / 2 } + fadeIn(tween(if (reduceMotion) 0 else 180))) togetherWith
            (slideOutVertically(tween(if (reduceMotion) 0 else 180)) { -it / 2 } + fadeOut(tween(if (reduceMotion) 0 else 120)))
    }, label = "currentLyricsLine") { index ->
        val line = index?.let { document?.lines?.getOrNull(it) }
        Column(Modifier.fillMaxWidth(), horizontalAlignment = Alignment.Start) {
            line?.let {
                if (it.cueLines.isNotEmpty()) it.cueLines.forEachIndexed { cueIndex, cueLine ->
                    val color = if (document?.agents?.firstOrNull { voice -> voice.id == cueLine.agentId }?.role == LyricsAgentRole.BACKGROUND)
                        MaterialTheme.colorScheme.secondary else selected
                    val cueHighlights = highlights[index.toUInt()]?.getOrNull(cueIndex)
                    LyricsText(cueLine.text, cueLine.reading, preferences,
                        FontFamily.Default, fontSize, normal, false, shadow, cueHighlights, color, cueRanges(cueLine), centered = false, maxLines = 1)
                    if (karaoke && preferences.showRomanization) {
                        cueLine.reading?.takeIf { reading -> reading.romanization.isNotBlank() }?.let { reading ->
                            HighlightedLyricsText(reading.romanization, romanizationRanges(reading), cueHighlights?.romanization.orEmpty(), color, maxLines = 1,
                                fontSize = 12.sp, color = normal, style = MaterialTheme.typography.bodyMedium.copy(shadow = shadow))
                        }
                    }
                } else LyricsText(it.text, it.reading, preferences, FontFamily.Default, fontSize, normal,
                    false, shadow, centered = false, maxLines = 1)
                if (preferences?.showRomanization == true &&
                    (!karaoke || it.cueLines.none { cue -> cue.reading?.romanization?.isNotBlank() == true })) {
                    val romanization = it.pronunciation ?: it.reading?.romanization
                    if (!romanization.isNullOrBlank()) Text(romanization, maxLines = 1, fontSize = 12.sp, color = normal,
                        style = MaterialTheme.typography.bodyMedium.copy(shadow = shadow))
                }
            }
        }
    }
}

@Composable
private fun rememberLyricsFrame(player: PlayerConnection, snapshot: AndroidLyricsSnapshot?, karaoke: Boolean,
    state: LazyListState? = null,
): State<AndroidLyricsFrame?> {
    val frame = remember(snapshot) { mutableStateOf<AndroidLyricsFrame?>(null) }
    val offset = player.lyricsOffset
    LaunchedEffect(snapshot, karaoke, offset, state) {
        if (snapshot == null) return@LaunchedEffect
        snapshotFlow {
            val lines = state?.layoutInfo?.visibleItemsInfo?.let { visible ->
                val first = visible.firstOrNull()?.index ?: 0
                first..(visible.lastOrNull()?.index ?: first)
            }
            player.playback?.transport to lines
        }.collectLatest { (transport, lines) ->
            if (transport == null) return@collectLatest
            while (true) {
                val position = player.positionNow()
                val started = SystemClock.elapsedRealtime()
                val next = withContext(Dispatchers.Default) {
                    snapshot.frame(position, offset, karaoke, currentLineOnly = lines == null,
                        firstLine = (lines?.first ?: 0).toUInt(), lastLine = (lines?.last ?: 0).toUInt())
                }
                frame.value = next
                if (transport.state != TransportStatus.PLAYING) break
                val updateAfter = next.updateAfterMillis ?: break
                val remaining = ceil(updateAfter.toDouble() / transport.playbackRate).toLong() -
                    (SystemClock.elapsedRealtime() - started)
                delay(remaining.coerceAtLeast(1L))
            }
        }
    }
    return frame
}

@Composable
private fun LyricsOffsetDialog(player: PlayerConnection, onDismiss: () -> Unit) {
    AlertDialog(onDismissRequest = onDismiss, title = { Text(translate("Lyrics offset (ms)")) }, text = {
        LyricsOffsetControls(player.lyricsOffset) { player.lyricsOffset = it }
    }, confirmButton = { TextButton(onDismiss) { Text(translate("Close")) } })
}

@Composable
private fun LyricsOffsetControls(value: Long, enabled: Boolean = true, onApply: (Long) -> Unit) {
    var text by remember(value) { mutableStateOf(value.toString()) }
    BoxWithConstraints(Modifier.fillMaxWidth()) {
        val fieldWidth = (maxWidth - 200.dp).coerceAtLeast(160.dp)
        Row(Modifier.horizontalScroll(rememberScrollState()), verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(2.dp)) {
            listOf(-100L, -50L).forEach { delta ->
                TextButton({ onApply((text.toLongOrNull() ?: value) + delta) }, Modifier.widthIn(min = 48.dp),
                    contentPadding = PaddingValues(horizontal = 2.dp), enabled = enabled) { Text(delta.toString(), maxLines = 1) }
            }
            OutlinedTextField(text, { input -> text = input; input.toLongOrNull()?.let(onApply) },
                colors = settingsTextFieldColors(),
                singleLine = true, enabled = enabled,
                label = { Text(translate("Lyrics offset (ms)"), maxLines = 1, overflow = TextOverflow.Ellipsis) },
                suffix = { Text("ms") },
                trailingIcon = { PlayerIconButton("rufin-view-refresh-symbolic", "Reset", { text = "0"; onApply(0L) }, enabled = enabled) },
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                modifier = Modifier.width(fieldWidth))
            listOf(50L, 100L).forEach { delta ->
                TextButton({ onApply((text.toLongOrNull() ?: value) + delta) }, Modifier.widthIn(min = 48.dp),
                    contentPadding = PaddingValues(horizontal = 2.dp), enabled = enabled) { Text("+$delta", maxLines = 1) }
            }
        }
    }
}

@Composable
private fun LyricsDocumentView(player: PlayerConnection, current: AndroidLyricsState, document: AndroidLyricsDocument,
    preferences: UserPreferences?, state: LazyListState, selection: List<Int>?, onSelect: (List<Int>) -> Unit,
    tools: LyricsTools?,
) {
    val snapshot = current.snapshot!!
    val shareLines = remember(document) { document.lines.map(::lyricsShareText) }
    val scope = rememberCoroutineScope()
    val karaoke = preferences?.karaokeMode == true
    val timing by rememberLyricsFrame(player, snapshot, karaoke, state)
    val highlights = timing?.highlights.orEmpty()
    val dragged by state.interactionSource.collectIsDraggedAsState()
    var pausedUntil by remember { mutableLongStateOf(0L) }
    var hadDrag by remember { mutableStateOf(false) }
    var followRevision by remember { mutableIntStateOf(0) }
    val pauseMillis = player.preferenceOptions?.lyricsScrollPauseMillis?.toLong() ?: 0L
    val reduceMotion = LocalReduceMotion.current
    LaunchedEffect(dragged) {
        if (dragged) hadDrag = true
        else if (hadDrag) {
            pausedUntil = SystemClock.elapsedRealtime() + pauseMillis
            delay(pauseMillis)
            followRevision++
        }
    }
    LaunchedEffect(current.mediaId) { hadDrag = false; pausedUntil = 0 }
    val active = timing?.activeLine?.toInt()
    val scroll = timing?.scrollLine?.toInt()
    // Following the song pauses while lines are picked.
    val selecting = selection != null
    LaunchedEffect(scroll, followRevision, selecting) {
        if (scroll != null && !selecting && !dragged && SystemClock.elapsedRealtime() >= pausedUntil) {
            if (state.layoutInfo.visibleItemsInfo.none { it.index == scroll }) {
                val center = (state.layoutInfo.viewportStartOffset + state.layoutInfo.viewportEndOffset) / 2
                if (reduceMotion) state.scrollToItem(scroll, -center)
                else state.animateScrollToItem(scroll, -center)
            }
            state.layoutInfo.visibleItemsInfo.firstOrNull { it.index == scroll }?.let { line ->
                val center = (state.layoutInfo.viewportStartOffset + state.layoutInfo.viewportEndOffset) / 2f
                val distance = line.offset + line.size / 2f - center
                if (reduceMotion) state.scrollBy(distance) else state.animateScrollBy(distance, tween(180))
            }
        }
    }
    val fontName = preferences?.fullscreenLyricsFontFamily
    val family = remember(fontName) { fontName?.let { FontFamily(Typeface.create(it, Typeface.NORMAL)) } ?: FontFamily.Default }
    val fontSize = preferences?.fullscreenLyricsFontSize?.toInt() ?: 19
    val fallbackColor = MaterialTheme.colorScheme.primary
    val selectedColor = preferences?.lyricsHighlightColor?.let(::playerLyricsColor) ?: fallbackColor
    val normal = MaterialTheme.colorScheme.onSurfaceVariant
    val shadow = Shadow(MaterialTheme.colorScheme.background, Offset.Zero, 4f)
    BoxWithConstraints(Modifier.fillMaxSize()) {
    LazyColumn(Modifier.fillMaxSize(), state = state, contentPadding = PaddingValues(horizontal = 24.dp, vertical = maxHeight / 2),
        verticalArrangement = Arrangement.spacedBy(18.dp)) {
        items(document.lines.size) { index ->
            val line = document.lines[index]
            val picked = selection?.contains(index) == true
            val lineActive = if (selection != null) picked else index == active || timing?.highlightAll == true
            val color = MaterialTheme.colorScheme.onSurface
            val opacity by animateFloatAsState(if (lineActive) 1f else .34f,
                tween(if (reduceMotion) 0 else 180), label = "lyricsOpacity")
            val scale = if (selection != null) 1f else if (lineActive) 1.04f else .96f
            val pick by animateColorAsState(MaterialTheme.colorScheme.onSurface.copy(alpha = if (picked) .12f else 0f),
                tween(if (reduceMotion) 0 else 180), label = "lyricsPick")
            Column(Modifier.fillMaxWidth().graphicsLayer {
                alpha = opacity
                scaleX = scale
                scaleY = scale
            }.clip(RoundedCornerShape(14.dp)).background(pick).then(if (selection != null) Modifier.clickable {
                onSelect(lyricsSelectionAfterTap(shareLines, selection, index))
            }.padding(vertical = 6.dp) else if (line.startMillis == null) Modifier else Modifier.clickable {
                // In the sync mode a tap says this line starts now; the offset follows it.
                if (tools?.syncing == true) {
                    player.lyricsOffset = lyricsOffsetForLineAt(line.startMillis!!, player.positionNow())
                    tools.picked = true
                    return@clickable
                }
                hadDrag = false
                pausedUntil = 0L
                scope.launch(start = kotlinx.coroutines.CoroutineStart.UNDISPATCHED) {
                    state.stopScroll()
                    if (index == scroll) followRevision++
                    current.currentMediaId?.let { player.seekFromLyrics(it, line.startMillis!!) }
                }
            }), horizontalAlignment = Alignment.CenterHorizontally) {
                if (line.cueLines.isNotEmpty()) line.cueLines.forEachIndexed { cueIndex, cueLine ->
                    val voice = document.agents.firstOrNull { it.id == cueLine.agentId }
                    val voiceColor = if (voice?.role == LyricsAgentRole.BACKGROUND) MaterialTheme.colorScheme.secondary else selectedColor
                    voice?.name?.let { Text(it, style = MaterialTheme.typography.labelSmall, color = normal) }
                    val cueHighlights = highlights[index.toUInt()]?.getOrNull(cueIndex)
                    LyricsText(cueLine.text, cueLine.reading, preferences, family, fontSize, color, lineActive, shadow, cueHighlights, voiceColor, cueRanges(cueLine))
                    if (karaoke && preferences.showRomanization) {
                        cueLine.reading?.takeIf { it.romanization.isNotBlank() }?.let { reading ->
                            HighlightedLyricsText(reading.romanization, romanizationRanges(reading), cueHighlights?.romanization.orEmpty(), voiceColor, Modifier.fillMaxWidth().padding(top = 6.dp),
                                fontFamily = family, fontSize = (fontSize * .7f).sp, color = color, textAlign = TextAlign.Center,
                                style = MaterialTheme.typography.bodyMedium.copy(shadow = shadow))
                        }
                    }
                } else LyricsText(line.text, line.reading, preferences, family, fontSize, color, lineActive, shadow)
                if (preferences?.showRomanization == true &&
                    (!karaoke || line.cueLines.none { it.reading?.romanization?.isNotBlank() == true })) {
                    val romanization = line.pronunciation ?: line.reading?.romanization
                    if (!romanization.isNullOrBlank()) Text(romanization, Modifier.padding(top = 6.dp), fontFamily = family,
                        fontSize = (fontSize * .7f).sp, color = color, textAlign = TextAlign.Center,
                        style = MaterialTheme.typography.bodyMedium.copy(shadow = shadow))
                }
            }
        }
    }
    }
}

private data class LyricsTextRange(val start: Int, val end: Int, val index: Int)

@Composable
private fun cueRanges(line: AndroidLyricsCueLine): List<LyricsTextRange> = remember(line) {
    line.cues.mapIndexedNotNull { index, cue ->
        val start = cue.utf16Start?.toInt() ?: return@mapIndexedNotNull null
        val end = cue.utf16End?.toInt() ?: return@mapIndexedNotNull null
        LyricsTextRange(start, end, index)
    }
}

@Composable
private fun romanizationRanges(reading: AndroidJapaneseReading): List<LyricsTextRange> = remember(reading) {
    reading.romanizationSpans.mapIndexed { index, span -> LyricsTextRange(span.start.toInt(), span.end.toInt(), index) }
}

@Composable
private fun HighlightedLyricsText(text: String, ranges: List<LyricsTextRange>, progress: List<Double>, highlightColor: Color,
    modifier: Modifier = Modifier, color: Color, style: TextStyle, fontFamily: FontFamily? = null,
    fontSize: TextUnit, fontWeight: FontWeight? = null, textAlign: TextAlign? = null, maxLines: Int = Int.MAX_VALUE,
) {
    var layout by remember(text) { mutableStateOf<TextLayoutResult?>(null) }
    val paths = remember(layout, ranges) { layout?.let { result -> ranges.map { result.getPathForRange(it.start, it.end) } }.orEmpty() }
    Text(text, modifier.drawWithContent {
        drawContent()
        layout?.let { result ->
            ranges.forEachIndexed { index, range ->
                val amount = progress.getOrNull(range.index) ?: return@forEachIndexed
                if (amount > 0.0) paths.getOrNull(index)?.let { path ->
                    clipPath(path) { drawText(result, color = lerp(color, highlightColor, amount.toFloat())) }
                }
            }
        }
    }, color = color, style = style, fontFamily = fontFamily, fontSize = fontSize,
        fontWeight = fontWeight, textAlign = textAlign, maxLines = maxLines, onTextLayout = { layout = it })
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun LyricsText(text: String, reading: AndroidJapaneseReading?, preferences: UserPreferences?,
    family: FontFamily, size: Int, color: Color, active: Boolean, shadow: Shadow,
    highlights: AndroidLyricsHighlight? = null, highlightColor: Color = color,
    ranges: List<LyricsTextRange> = emptyList(), centered: Boolean = true, maxLines: Int = Int.MAX_VALUE,
) {
    val style = MaterialTheme.typography.bodyMedium.copy(shadow = shadow)
    val weight = if (active) FontWeight.ExtraBold else FontWeight.SemiBold
    if (preferences?.showFurigana == true && reading?.segments?.isNotEmpty() == true) {
        val segmentRanges = remember(reading, ranges) {
            var cursor = 0
            reading.segments.map { segment ->
                val start = cursor
                cursor += segment.surface.length
                ranges.mapNotNull { range ->
                    val from = maxOf(start, range.start)
                    val to = minOf(cursor, range.end)
                    if (from < to) LyricsTextRange(from - start, to - start, range.index) else null
                }
            }
        }
        FlowRow(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(1.dp,
            if (centered) Alignment.CenterHorizontally else Alignment.Start), maxLines = maxLines) {
            reading.segments.forEachIndexed { index, segment ->
                Column(horizontalAlignment = Alignment.CenterHorizontally) {
                    val progress = highlights?.furigana?.getOrNull(index)
                    Text(segment.furigana.orEmpty(), fontFamily = family, fontSize = (size * .5f).sp,
                        color = progress?.let { lerp(color, highlightColor, it.toFloat()) } ?: color, style = style)
                    HighlightedLyricsText(segment.surface, segmentRanges[index], highlights?.cues.orEmpty(), highlightColor,
                        fontFamily = family, fontSize = size.sp, color = color, style = style, fontWeight = weight)
                }
            }
        }
    } else HighlightedLyricsText(text, ranges, highlights?.cues.orEmpty(), highlightColor,
        Modifier.fillMaxWidth(), fontFamily = family, fontSize = size.sp, color = color,
        textAlign = if (centered) TextAlign.Center else TextAlign.Start, maxLines = maxLines,
        style = style, fontWeight = weight)
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun LyricsSearchSheet(player: PlayerConnection, token: CurrentMediaId, onEdit: () -> Unit, onSave: () -> Unit, onDismiss: () -> Unit) {
    val current = player.currentLyrics
    val document = current?.document
    var artist by rememberSaveable(token) { mutableStateOf(player.currentItem?.metadata?.artist.orEmpty()) }
    var title by rememberSaveable(token) { mutableStateOf(player.currentItem?.metadata?.title.orEmpty()) }
    val matchingSearch = player.lyricsSearch?.let { it.first == token && it.second.artistName == artist.trim() && it.second.trackName == title.trim() } == true
    val results = if (matchingSearch) player.lyricsResults else null
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(Modifier.fillMaxWidth().imePadding().padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text(translate("Search Lyrics"), style = MaterialTheme.typography.titleLarge)
            // The track's other lyrics actions: its editor, Save Lyrics, and dropping what was fetched.
            val shade = MaterialTheme.colorScheme.onSurface.copy(alpha = .08f)
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                PlayerMenuTile("rufin-document-edit-symbolic", translate("Edit Lyrics"), Modifier.weight(1f),
                    enabled = document != null && current.writable, container = shade, onClick = onEdit)
                PlayerMenuTile("rufin-download-symbolic", translate("Save Lyrics"), Modifier.weight(1f),
                    enabled = document != null, container = shade, onClick = onSave)
                PlayerMenuTile("rufin-process-stop-symbolic", translate("Clear"), Modifier.weight(1f),
                    enabled = current?.clearable == true, container = shade, onClick = { player.clearLyrics(token); onDismiss() })
            }
            OutlinedTextField(artist, { artist = it }, label = { Text(translate("Artist")) }, singleLine = true, modifier = Modifier.fillMaxWidth(), colors = settingsTextFieldColors())
            OutlinedTextField(title, { title = it }, label = { Text(translate("Track")) }, singleLine = true, modifier = Modifier.fillMaxWidth(), colors = settingsTextFieldColors())
            Button({ player.searchLyrics(token, artist, title) }, enabled = player.lyricsToken == token && player.settings?.application?.privateMode != true) { Text(translate("Search")) }
            if (matchingSearch && player.lyricsSearching) CircularProgressIndicator()
            LazyColumn(Modifier.fillMaxWidth().heightIn(max = 360.dp)) {
                items(results.orEmpty(), key = { "${it.provider}:${it.id}" }) { hit ->
                    ListItem(headlineContent = { Text(hit.trackName) }, supportingContent = { Text("${hit.artistName} · ${hit.albumName}\n${externalLyricsProviderTitle(hit.provider)}") },
                        modifier = Modifier.clickable { player.previewLyrics(token, hit); onDismiss() })
                }
                if (matchingSearch && !player.lyricsSearching && results?.isEmpty() == true) item {
                    Text(translate("No results"), Modifier.padding(16.dp))
                }
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun LyricsEditSheet(player: PlayerConnection, token: CurrentMediaId, onDismiss: () -> Unit) {
    var text by rememberSaveable(token) { mutableStateOf("") }
    var appliedOffset by rememberSaveable(token) { mutableLongStateOf(player.lyricsOffset) }
    var loaded by rememberSaveable(token) { mutableStateOf(false) }
    LaunchedEffect(token) {
        if (loaded) return@LaunchedEffect
        try { text = player.lyricsText(token).orEmpty(); loaded = true }
        catch (cancelled: CancellationException) { throw cancelled }
        catch (error: Exception) { player.reportError(error) }
    }
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
        Column(Modifier.fillMaxWidth().fillMaxHeight(.9f).imePadding().padding(horizontal = 20.dp, vertical = 12.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Text(translate("Edit Lyrics"), Modifier.weight(1f), style = MaterialTheme.typography.titleMedium)
                TextButton({ player.editLyrics(token, text); onDismiss() },
                    enabled = loaded && player.lyricsToken == token && text.isNotBlank()) { Text(translate("Save")) }
            }
            if (!loaded) LinearProgressIndicator(Modifier.fillMaxWidth())
            LyricsOffsetControls(appliedOffset, loaded) { offset ->
                val delta = (offset.toBigInteger() - appliedOffset.toBigInteger()).coerceIn(Long.MIN_VALUE.toBigInteger(), Long.MAX_VALUE.toBigInteger()).toLong()
                val shifted = shiftLrcTextTimestamps(text, delta)
                text = shifted
                appliedOffset = offset
                player.lyricsOffset = offset
            }
            OutlinedTextField(text, { text = it }, modifier = Modifier.fillMaxWidth().weight(1f),
                colors = settingsTextFieldColors(),
                textStyle = MaterialTheme.typography.bodyMedium.copy(fontFamily = FontFamily.Monospace),
                keyboardOptions = KeyboardOptions(autoCorrectEnabled = false), enabled = loaded)
        }
    }
}
