package app.rufin.rufin.settings

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.ColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ColorFilter
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.clipPath
import androidx.compose.ui.graphics.drawscope.translate
import androidx.compose.ui.graphics.painter.Painter
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.unit.dp
import app.rufin.rufin.core.AndroidThemeOption
import app.rufin.rufin.rufinIconResources

/** The preview's shape: its drawing is 93 units wide and 104 high. */
internal const val THEME_PREVIEW_RATIO = 93f / 104f

/** A theme tile's picture: the top of a library page. Two themes (System's light and dark) share the tile along its diagonal. */
@Composable
internal fun ThemePreview(themes: List<AndroidThemeOption>, systemAccent: Boolean, modifier: Modifier = Modifier) {
    val schemes = themes.map { theme ->
        val defaults = themeDefaults(theme.dark, systemAccent)
        remember(theme.colors, defaults, systemAccent) { resolveThemeScheme(ThemeColors(theme.colors), defaults, systemAccent) }
    }
    val menu = painterResource(rufinIconResources.getValue("rufin-open-menu-symbolic").drawable)
    val search = painterResource(rufinIconResources.getValue("rufin-search-symbolic").drawable)
    Canvas(modifier.clip(RoundedCornerShape(8.dp)).clearAndSetSemantics { }) {
        if (schemes.size == 1) page(schemes[0], menu, search)
        else schemes.forEachIndexed { index, scheme ->
            val half = Path().apply {
                moveTo(size.width, 0f)
                lineTo(0f, size.height)
                if (index == 0) lineTo(0f, 0f) else lineTo(size.width, size.height)
                close()
            }
            clipPath(half) { page(scheme, menu, search) }
        }
    }
}

private fun DrawScope.page(scheme: ColorScheme, menu: Painter, search: Painter) {
    val unit = size.width / 93f
    val text = scheme.onSurface
    fun box(color: Color, x: Float, y: Float, width: Float, height: Float, radius: Float = height / 2) =
        drawRoundRect(color, Offset(x * unit, y * unit), Size(width * unit, height * unit), CornerRadius(radius * unit))
    fun icon(painter: Painter, x: Float) = translate(x * unit, 7f * unit) {
        with(painter) { draw(Size(9f * unit, 9f * unit), colorFilter = ColorFilter.tint(text)) }
    }
    drawRect(scheme.surface)
    icon(menu, 7f)
    box(text.copy(alpha = .95f), 23f, 9.5f, 27f, 4f)
    icon(search, 77f)
    box(scheme.primaryContainer, 6f, 24f, 31f, 13f)
    box(text.copy(alpha = .95f), 12.5f, 29f, 18f, 3f)
    box(text.copy(alpha = .55f), 44f, 29f, 17f, 3f)
    box(text.copy(alpha = .55f), 69f, 29f, 24f, 3f)
    val cover = 37.5f
    repeat(2) { column -> repeat(2) { row ->
        val x = 6f + column * (cover + 6f)
        val y = 44f + row * (cover + 19f)
        // The second cover is the playing one: its title takes the accent.
        val title = if (column == 1 && row == 0) scheme.primary else text.copy(alpha = .9f)
        box(text.copy(alpha = .2f), x, y, cover, cover, 5f)
        box(title, x, y + cover + 5f, cover * if (column == 0) .75f else .55f, 3f)
        box(text.copy(alpha = .5f), x, y + cover + 11f, cover * .45f, 2.5f)
    } }
}
