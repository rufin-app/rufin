package app.rufin.rufin.settings

import app.rufin.rufin.ui.PageLoading

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.GridItemSpan
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import app.rufin.rufin.shared.rufin_core.AccentPreference
import app.rufin.rufin.shared.rufin_core.ThemePreference
import app.rufin.rufin.core.translate
import app.rufin.rufin.ui.LocalPlayerBottomPadding
import app.rufin.rufin.ui.RufinIcon

@Composable
internal fun ThemeSettingsPage(preferences: PreferencesConnection) {
    val settings = preferences.state
    if (settings == null) {
        PageLoading()
        return
    }
    val systemDark = isSystemInDarkTheme()
    val themes = listOf(ThemePreference.System to translate("System"), ThemePreference.Light to translate("Light"),
        ThemePreference.Dark to translate("Dark")) +
        settings.themes.filter { it.id != "builtin:light" && it.id != "builtin:dark" }.map { ThemePreference.Named(it.id) to it.name }
    LazyVerticalGrid(GridCells.Adaptive(100.dp), Modifier.fillMaxSize(),
        contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = 8.dp, bottom = 24.dp + LocalPlayerBottomPadding.current),
        horizontalArrangement = Arrangement.spacedBy(10.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        items(themes, key = { it.second + it.first.toString() }) { (choice, title) ->
            val themeId = when (choice) {
                is ThemePreference.System -> if (systemDark) "builtin:dark" else "builtin:light"
                is ThemePreference.Light -> "builtin:light"
                is ThemePreference.Dark -> "builtin:dark"
                is ThemePreference.Named -> choice.v1
            }
            val theme = settings.themes.find { it.id == themeId }
            val selected = settings.theme == choice
            val standard = choice !is ThemePreference.Named
            val colors = MaterialTheme.colorScheme
            // One container per theme: its picture, then its name and accent.
            Surface(Modifier.fillMaxWidth().selectable(selected, role = Role.RadioButton) { preferences.selectTheme(choice) }
                .semantics { contentDescription = title },
                shape = RoundedCornerShape(12.dp), color = colors.surfaceContainer,
                border = BorderStroke(if (selected) 2.dp else 1.dp, if (selected) colors.primary else colors.outlineVariant)) {
                Column {
                    val shown = if (choice is ThemePreference.System) settings.themes.filter { it.id == "builtin:light" || it.id == "builtin:dark" }
                        .sortedBy { it.dark } else listOfNotNull(theme)
                    ThemePreview(shown, standard && settings.accent == AccentPreference.SYSTEM,
                        Modifier.fillMaxWidth().aspectRatio(THEME_PREVIEW_RATIO))
                    Row(Modifier.fillMaxWidth().heightIn(min = 48.dp), verticalAlignment = Alignment.CenterVertically) {
                        Text(title, Modifier.weight(1f).padding(start = 8.dp),
                            style = MaterialTheme.typography.labelMedium, maxLines = 2, overflow = TextOverflow.Ellipsis)
                        if (standard) ThemeAccentMenu(
                            settings.accents.map { AccentEntry(translate(it.title), it.color, it.accent == settings.accent) { preferences.selectAccent(it.accent) } },
                            theme?.dark ?: systemDark)
                        else if ((theme?.accents?.size ?: 0) > 1) ThemeAccentMenu(
                            theme!!.accents.map { AccentEntry(it.name, it.color, it.id == theme.accentId) { preferences.selectThemeAccent(themeId, it.id) } },
                            theme.dark)
                    }
                }
            }
        }
        items(settings.errors + preferences.themeErrors, span = { GridItemSpan(maxLineSpan) }) {
            app.rufin.rufin.ui.ErrorNotice(it)
        }
    }
}

/** One accent color a menu offers. */
private class AccentEntry(val name: String, val color: String?, val selected: Boolean, val select: () -> Unit)

@Composable
private fun ThemeAccentMenu(options: List<AccentEntry>, dark: Boolean) {
    var expanded by remember { mutableStateOf(false) }
    val current = options.find { it.selected } ?: options.first()
    val label = translate("Accent color")
    Box {
        IconButton({ expanded = true }, Modifier.semantics {
            contentDescription = label
            stateDescription = current.name
        }) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(3.dp)) {
                AccentSwatch(current.color, dark)
                RufinIcon("rufin-go-down-symbolic", null, Modifier.size(12.dp))
            }
        }
        DropdownMenu(expanded, { expanded = false }) {
            options.forEach { option ->
                DropdownMenuItem(
                    text = { Text(option.name) },
                    leadingIcon = { AccentSwatch(option.color, dark) },
                    trailingIcon = {
                        if (option === current) RufinIcon("rufin-object-select-symbolic", null, Modifier.size(18.dp))
                    },
                    onClick = { option.select(); expanded = false },
                )
            }
        }
    }
}

@Composable
private fun AccentSwatch(value: String?, dark: Boolean) {
    val color = remember(value) { value?.let { ThemeColors(mapOf("accent" to it)).named("accent") } }
        ?: themeDefaults(dark, systemAccent = true).primary
    Surface(Modifier.size(16.dp), CircleShape, color = color) {}
}
