package app.rufin.rufin.player

import android.os.SystemClock
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.core.tween
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.ui.Alignment
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import app.rufin.rufin.core.translate
import app.rufin.rufin.settings.LocalReduceMotion
import kotlinx.coroutines.delay
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.clipRect
import androidx.compose.ui.graphics.drawscope.withTransform
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.unit.IntSize
import androidx.compose.material3.MaterialTheme
import app.rufin.rufin.core.AndroidRgb
import app.rufin.rufin.core.AndroidVisualizerDrawing
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.isActive
import kotlin.math.PI
import kotlin.math.cos
import kotlin.math.floor
import kotlin.math.min
import kotlin.math.sin

/** The visualizer with its presets in reach: a tap shows their stepper, which goes again once left alone. */
@Composable
internal fun VisualizerPresets(player: PlayerConnection, modifier: Modifier = Modifier) {
    val visualizer = player.settings?.visualizer
    val presets = visualizer?.presets.orEmpty()
    val slot = visualizer?.selectedPreset?.toInt()
    var touches by remember { mutableIntStateOf(0) }
    var shown by remember { mutableStateOf(false) }
    LaunchedEffect(touches) {
        if (touches == 0) return@LaunchedEffect
        shown = true
        delay(3_000)
        shown = false
    }
    fun step(by: Int) {
        touches++
        if (presets.isEmpty()) return
        val next = if (slot == null) (if (by > 0) 0 else presets.lastIndex) else Math.floorMod(slot + by, presets.size)
        player.visualizerPreset(next.toULong(), false)
    }
    val fade = tween<Float>(if (LocalReduceMotion.current) 0 else 180)
    Box(modifier.clickable(remember { MutableInteractionSource() }, indication = null, onClickLabel = translate("Preset")) { touches++ }) {
        VisualizerView(player, Modifier.matchParentSize())
        AnimatedVisibility(shown, Modifier.align(Alignment.BottomCenter).padding(bottom = 18.dp), enter = fadeIn(fade), exit = fadeOut(fade)) {
            Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Surface(shape = RoundedCornerShape(24.dp), color = MaterialTheme.colorScheme.surfaceContainerHigh) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        PlayerIconButton("rufin-go-previous-symbolic", "Previous", { step(-1) }, iconSize = 16.dp)
                        // The built-in presets' names are translated; a saved one keeps the name it was given.
                        Text(presets.getOrNull(slot ?: -1)?.let { if (slot!! < BuiltInPresets) translate(it.name) else it.name } ?: translate("Custom"),
                            Modifier.widthIn(max = 200.dp), maxLines = 1, overflow = TextOverflow.Ellipsis,
                            style = MaterialTheme.typography.labelLarge, fontWeight = FontWeight.SemiBold)
                        PlayerIconButton("rufin-go-next-symbolic", "Next", { step(1) }, iconSize = 16.dp)
                    }
                }
                // One dot a preset, while they fit in a glance.
                if (presets.size in 2..12) Row(horizontalArrangement = Arrangement.spacedBy(5.dp)) {
                    presets.indices.forEach { index ->
                        Box(Modifier.size(6.dp).background(MaterialTheme.colorScheme.onSurface.copy(alpha = if (index == slot) 1f else .3f), CircleShape))
                    }
                }
            }
        }
    }
}

/** The presets Rufin ships, which come first in the list. */
internal const val BuiltInPresets = 7

@Composable
internal fun VisualizerView(player: PlayerConnection, modifier: Modifier = Modifier, opacity: Float = 1f) {
    var bounds by remember { mutableStateOf(IntSize.Zero) }
    var drawing by remember { mutableStateOf<AndroidVisualizerDrawing?>(null) }
    val accent = MaterialTheme.colorScheme.primary
    val config = player.settings?.visualizer
    val frameLimit = config?.fpsLimit?.toInt()
    val staleNanos = (player.preferenceOptions?.visualizerStaleMicros?.toLong() ?: 0L) * 1000
    LaunchedEffect(player.bridge, bounds, accent, frameLimit) {
        if (bounds.width == 0 || bounds.height == 0) return@LaunchedEffect
        val rgb = AndroidRgb(accent.red, accent.green, accent.blue)
        snapshotFlow { player.levels.isNotEmpty() }.collectLatest { active ->
            if (!active) {
                player.drawing(emptyList(), bounds.width.toFloat(), bounds.height.toFloat(), 0L, rgb)
                drawing = null
                return@collectLatest
            }
            var deadline = 0.0
            while (isActive) {
                val now = withFrameNanos { it }
                if (now < deadline) continue
                val target = if (SystemClock.elapsedRealtimeNanos() - player.visualizerObservedAt >= staleNanos)
                    List(player.levels.size) { 0.0 } else player.levels
                drawing = player.drawing(target, bounds.width.toFloat(), bounds.height.toFloat(), now, rgb)
                val fps = drawing?.fpsLimit?.toInt()?.coerceIn(1, 180) ?: 30
                val interval = 1_000_000_000.0 / fps
                val scheduled = if (deadline == 0.0) now.toDouble() else deadline
                deadline = (scheduled + interval).coerceAtLeast(now.toDouble())
            }
        }
    }
    Canvas(modifier.fillMaxSize().onSizeChanged { bounds = it }) {
        val frame = drawing ?: return@Canvas
        if (frame.bars.isEmpty()) return@Canvas
        val mainHeight = frame.mainHeight.toFloat()
        val shadowHeight = frame.shadowHeight.toFloat()
        clipRect {
            drawVisualizer(frame, mainHeight, opacity)
            if (shadowHeight > 0f) {
                withTransform({
                    translate(0f, mainHeight + shadowHeight)
                    scale(1f, -shadowHeight / mainHeight, pivot = Offset.Zero)
                }) {
                    drawVisualizer(frame, mainHeight, opacity * frame.shadowOpacity.toFloat(), frame.shadowBrightness.toFloat())
                }
            }
        }
    }
}

private fun DrawScope.drawVisualizer(frame: AndroidVisualizerDrawing, height: Float, opacity: Float, brightness: Float = 1f) {
    val cell = frame.cell.toFloat()
    val gap = frame.gap.toFloat()
    val rowHeight = frame.rowHeight.toFloat()
    val rows = frame.rows.toInt()
    val rowStride = rowHeight + 2f
    val graphHeight = rows * rowStride - 2f
    val bottom = height
    val left = size.width * .008f
    val alpha = frame.opacity.toFloat() * opacity
    fun color(mix: Float, alphaScale: Float = 1f, brightnessScale: Float = 1f): Color = Color(
        ((frame.startColor.red + (frame.endColor.red - frame.startColor.red) * mix) * brightness * brightnessScale).coerceIn(0f, 1f),
        ((frame.startColor.green + (frame.endColor.green - frame.startColor.green) * mix) * brightness * brightnessScale).coerceIn(0f, 1f),
        ((frame.startColor.blue + (frame.endColor.blue - frame.startColor.blue) * mix) * brightness * brightnessScale).coerceIn(0f, 1f),
        alpha * alphaScale,
    )
    val gradient = Brush.verticalGradient(listOf(color(1f), color(0f)), startY = bottom - graphHeight, endY = bottom)
    if (frame.style in listOf("line", "filled", "ribbon", "halo")) {
        val ribbon = frame.style == "ribbon"
        val halo = frame.style == "halo"
        fun points(inward: Boolean = false) = frame.bars.mapIndexed { index, level ->
            if (halo) {
                val radius = min(size.width, height) * .22f + if (inward)
                    -min(level.toFloat() * frame.inwardShadowScale.toFloat(), frame.inwardShadowLimit.toFloat())
                    else level.toFloat() * min(size.width, height) * .25f
                val angle = (index.toDouble() + if (frame.mirrored) .5 else 0.0) / frame.bars.size * 2 * PI - PI / 2
                Offset(size.width / 2 + radius * cos(angle).toFloat(), height / 2 + radius * sin(angle).toFloat())
            } else Offset(left + cell / 2 + index * (cell + gap),
                if (ribbon) height / 2 - level.toFloat() * graphHeight / 2 else bottom - level.toFloat() * graphHeight)
        }
        val outline = points()
        val path = if (ribbon || halo) smoothClosedPath(
            if (ribbon) outline + outline.asReversed().map { Offset(it.x, height - it.y) } else outline
        ) else Path().apply {
            outline.forEachIndexed { index, point ->
                if (index == 0) moveTo(point.x, point.y) else lineTo(point.x, point.y)
            }
        }
        if (frame.style == "filled") {
            path.lineTo(left + cell / 2 + (frame.bars.size - 1) * (cell + gap), bottom)
            path.lineTo(left + cell / 2, bottom); path.close()
            drawPath(path, gradient)
        } else if (ribbon) drawPath(path, gradient)
        else {
            drawPath(path, gradient, style = Stroke(2f))
            if (halo && frame.inwardShadowLimit > 0) {
                drawPath(smoothClosedPath(points(true)), color(.5f, frame.shadowOpacity.toFloat(), frame.shadowBrightness.toFloat()), style = Stroke(2f))
            }
        }
    }
    frame.bars.forEachIndexed { column, value ->
        val level = value.toFloat()
        val x = left + column * (cell + gap)
        val barHeight = level * graphHeight
        if (frame.style == "circular" || frame.style == "halo") {
            val center = Offset(size.width / 2, height / 2)
            val radius = min(size.width, height) * .22f
            val extent = min(size.width, height) * .25f
            val angle = (column.toDouble() + if (frame.mirrored) .5 else 0.0) / frame.bars.size * 2 * PI - PI / 2
            val direction = Offset(cos(angle).toFloat(), sin(angle).toFloat())
            val thickness = (2 * PI * radius / frame.bars.size - gap).toFloat().coerceAtLeast(1f)
            val shadowLength = min(level * frame.inwardShadowScale.toFloat(), frame.inwardShadowLimit.toFloat())
            val shadowColor = color(level, frame.shadowOpacity.toFloat(), frame.shadowBrightness.toFloat())
            if (frame.style == "circular" && shadowLength > 0) drawLine(shadowColor, center + direction * radius,
                center + direction * (radius - shadowLength), strokeWidth = thickness, cap = StrokeCap.Butt)
            if (frame.style == "circular" && level > 0) drawLine(color(level), center + direction * radius,
                center + direction * (radius + extent * level), strokeWidth = thickness, cap = StrokeCap.Butt)
            frame.peaks.getOrNull(column)?.takeIf { it > 0 }?.let { peak ->
                drawCircle(color(level), 1.5f, center + direction * (radius + extent * peak.toFloat()))
                val shadowPeak = peak.toFloat() * frame.inwardShadowScale.toFloat()
                if (shadowPeak > 0 && shadowPeak <= frame.inwardShadowLimit.toFloat()) {
                    drawCircle(shadowColor, 1.5f, center + direction * (radius - shadowPeak))
                }
            }
        } else {
            if (frame.style == "segmented") {
                val scaled = level * rows
                val full = floor(scaled).toInt().coerceIn(0, rows)
                repeat(full) { row ->
                    val mix = if (full > 1) row.toFloat() / (full - 1) else 0f
                    drawRect(color(mix, .72f + mix * .24f), Offset(x, bottom - rowHeight - row * rowStride), Size(cell, rowHeight))
                }
                val part = scaled - floor(scaled)
                if (full < rows && part >= .14f) {
                    val mix = full.toFloat() / (rows - 1).coerceAtLeast(1)
                    drawRect(color(mix, part * .76f), Offset(x, bottom - rowHeight - full * rowStride), Size(cell, rowHeight))
                }
            } else if (barHeight > 0) {
                val y = if (frame.style == "mirrored" || frame.style == "ribbon") height / 2 - barHeight / 2 else bottom - barHeight
                when (frame.style) {
                    "rounded" -> drawRoundRect(gradient, Offset(x, y), Size(cell, barHeight), CornerRadius(min(cell, barHeight) / 2))
                    "outline" -> drawRect(gradient, Offset(x, y), Size(cell, barHeight), style = Stroke(1f))
                    "solid", "mirrored" -> drawRect(gradient, Offset(x, y), Size(cell, barHeight))
                }
            }
            frame.peaks.getOrNull(column)?.takeIf { it > 0 }?.let { peak ->
                val y = if (frame.style == "mirrored" || frame.style == "ribbon") height / 2 - peak.toFloat() * graphHeight / 2 else bottom - peak.toFloat() * graphHeight
                drawRect(color(1f), Offset(x, y), Size(cell, 2f))
                if (frame.style == "mirrored" || frame.style == "ribbon") drawRect(color(1f), Offset(x, height - y - 2), Size(cell, 2f))
            }
        }
    }
}

private fun smoothClosedPath(points: List<Offset>) = Path().apply {
    val start = (points.last() + points.first()) / 2f
    moveTo(start.x, start.y)
    points.forEachIndexed { index, point ->
        val end = (point + points[(index + 1) % points.size]) / 2f
        quadraticTo(point.x, point.y, end.x, end.y)
    }
    close()
}
