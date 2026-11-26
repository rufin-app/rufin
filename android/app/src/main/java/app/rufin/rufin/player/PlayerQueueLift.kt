package app.rufin.rufin.player

import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.snap
import androidx.compose.animation.core.spring
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.compositeOver
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.nestedscroll.NestedScrollConnection
import androidx.compose.ui.input.nestedscroll.NestedScrollSource
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.Velocity
import androidx.compose.ui.unit.dp
import app.rufin.rufin.shared.rufin_core.Route
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.launch
import kotlin.math.abs

/** The queue under the phone's player, by its stage: 0 closed, 1 open under the squeezed player, 2 expanded
 *  under the player's bar. A screen too short to squeeze the player has the one stop, 2. */
@Stable
internal class QueueLift(private val scope: CoroutineScope) {
    val stage = Animatable(0f).apply { updateBounds(0f, 2f) }
    /** The sheet's height in view at stage 1, and at stage 2. */
    var openPx by mutableFloatStateOf(0f)
    var fullPx by mutableFloatStateOf(0f)
    var short by mutableStateOf(false)
    var reduceMotion = false
    val shown by derivedStateOf { stage.value > 0f }
    val folding by derivedStateOf { fold() > 0f }
    private val stops get() = if (short) listOf(0f, 2f) else listOf(0f, 1f, 2f)

    /** How far the player is squeezed above the open queue, 0 to 1. */
    fun squeeze() = if (short) 0f else stage.value.coerceAtMost(1f)
    /** How far the player has folded into its bar, 0 to 1. */
    fun fold() = if (short) stage.value / 2f else (stage.value - 1f).coerceAtLeast(0f)
    /** The height of the sheet in view. */
    fun sheetPx() = if (short) fullPx * fold() else openPx * squeeze() + (fullPx - openPx) * fold()

    /** Follows a drag, a finger moving down being positive. */
    fun dragBy(amount: Float) {
        val value = stage.value
        val span = if (short) fullPx / 2f else if (value < 1f || value == 1f && amount > 0f) openPx else fullPx - openPx
        if (span <= 0f) return
        scope.launch(start = CoroutineStart.UNDISPATCHED) { stage.snapTo((value - amount / span).coerceIn(0f, 2f)) }
    }

    /** Settles on a stop: the next one the way the drag went, from [from] or by its [velocity], else the nearest. */
    fun settle(from: Float = stage.value, velocity: Float = 0f) {
        val value = stage.value
        moveTo(when {
            velocity < -400f || velocity <= 400f && value - from > .15f -> stops.first { it >= value }
            velocity > 400f || from - value > .15f -> stops.last { it <= value }
            else -> stops.minBy { abs(it - value) }
        })
    }

    fun open() = moveTo(stops[1])
    fun close() = moveTo(0f)
    private fun moveTo(target: Float) {
        scope.launch { stage.animateTo(target, if (reduceMotion) snap() else spring(stiffness = 420f)) }
    }

    /** The list's drags: one upwards raises the sheet before the list scrolls, one downwards lowers it once
     *  the list is at its top. */
    val scroll = object : NestedScrollConnection {
        override fun onPreScroll(available: Offset, source: NestedScrollSource): Offset {
            if (source != NestedScrollSource.UserInput || available.y >= 0f || stage.value >= 2f) return Offset.Zero
            dragBy(available.y)
            return Offset(0f, available.y)
        }
        override fun onPostScroll(consumed: Offset, available: Offset, source: NestedScrollSource): Offset {
            if (source != NestedScrollSource.UserInput || available.y <= 0f) return Offset.Zero
            dragBy(available.y)
            return Offset(0f, available.y)
        }
        override suspend fun onPreFling(available: Velocity): Velocity {
            if (stage.value in stops) return Velocity.Zero
            settle(velocity = available.y)
            return available
        }
    }
}

/** The queue's sheet under the player and, over the sheet's end, the bar of its modes. */
@Composable
internal fun BoxScope.PlayerQueueLift(player: PlayerConnection, lift: QueueLift, onAutoDjSettings: () -> Unit,
    onNavigate: (Route, String, String?) -> Unit) {
    val base = MaterialTheme.colorScheme
    val surface = base.surface
    // The player's shades are made for its gradient and vanish on the sheet's own fill: here the rows and the
    // buttons are lighter than the sheet, as the wide player's pane sets its tabs.
    MaterialTheme(base.copy(surfaceContainer = base.onSurface.copy(alpha = .10f).compositeOver(surface),
        surfaceContainerHigh = base.onSurface.copy(alpha = .17f).compositeOver(surface),
        secondaryContainer = base.primary.copy(alpha = .32f).compositeOver(surface))) {
        Surface(Modifier.align(Alignment.BottomCenter).fillMaxWidth().height(with(LocalDensity.current) { lift.fullPx.toDp() })
            .graphicsLayer { translationY = lift.fullPx - lift.sheetPx() },
            shape = RoundedCornerShape(topStart = 28.dp, topEnd = 28.dp), color = surface, shadowElevation = 12.dp) {
            Column {
                Box(Modifier.padding(top = 10.dp, bottom = 2.dp).size(32.dp, 4.dp).align(Alignment.CenterHorizontally)
                    .background(MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = .5f), CircleShape))
                PlayerQueue(player, Modifier.fillMaxSize().navigationBarsPadding(), sheet = true, modes = false, bottomSpace = 86.dp,
                    listScroll = lift.scroll, onNavigate = onNavigate)
            }
        }
        // The bar stands at the screen's end at either stop, on a fade of the sheet's colour, so it cuts no row.
        Box(Modifier.align(Alignment.BottomCenter).fillMaxWidth().graphicsLayer { alpha = ((lift.stage.value - .4f) / .6f).coerceIn(0f, 1f) }
            .drawBehind {
                val solid = (26.dp.toPx() / size.height).coerceIn(0f, 1f)
                drawRect(Brush.verticalGradient(0f to Color.Transparent, solid to surface, 1f to surface))
            }.navigationBarsPadding().padding(start = 12.dp, end = 12.dp, top = 30.dp, bottom = 12.dp)) {
            QueueModes(player, Modifier.fillMaxWidth(), transport = lift::fold, onAutoDjSettings = onAutoDjSettings)
        }
    }
}
