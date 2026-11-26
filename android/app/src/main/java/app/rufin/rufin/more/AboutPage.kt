package app.rufin.rufin.more

import android.content.ActivityNotFoundException
import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.Image
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import app.rufin.rufin.BuildConfig
import app.rufin.rufin.R
import app.rufin.rufin.core.about
import app.rufin.rufin.core.translate
import app.rufin.rufin.settings.SettingsGroup
import app.rufin.rufin.settings.SettingsIcon
import app.rufin.rufin.settings.SettingsLeads
import app.rufin.rufin.settings.SettingsPage
import app.rufin.rufin.settings.SettingsRow

/** About Rufin: the app and its version, the pages about the app itself, the project's links, and who made it
 *  and under which licence. A wide page puts the app beside the lists. */
@Composable
internal fun AboutPage(navigate: (MoreDestination) -> Unit) {
    val facts = remember { about() }
    val context = LocalContext.current
    val scheme = MaterialTheme.colorScheme
    var details by remember { mutableStateOf<Pair<String, String>?>(null) }
    val app: @Composable (Modifier) -> Unit = { modifier ->
        Column(modifier, horizontalAlignment = Alignment.CenterHorizontally) {
            Image(painterResource(R.drawable.app_icon), null, Modifier.padding(bottom = 12.dp).size(88.dp))
            Text("Rufin", style = MaterialTheme.typography.headlineSmall, fontWeight = FontWeight.SemiBold)
            Text("v${BuildConfig.VERSION_NAME}", Modifier.padding(top = 2.dp), style = MaterialTheme.typography.bodyMedium,
                color = scheme.onSurfaceVariant)
            Text(translate("Thank you for trying out Rufin! If you have problems or suggestions, please open an issue in Github."),
                Modifier.padding(top = 12.dp).widthIn(max = 300.dp), style = MaterialTheme.typography.bodyMedium,
                color = scheme.onSurfaceVariant, textAlign = TextAlign.Center)
        }
    }
    val rows: @Composable ColumnScope.() -> Unit = {
        SettingsGroup {
            listOf(MoreDestination.Overview, MoreDestination.Versions, MoreDestination.Logs).forEach { page ->
                SettingsRow(translate(page.title), leading = { SettingsIcon(checkNotNull(page.icon)) }, trailing = { SettingsLeads() },
                    modifier = Modifier.clickable { navigate(page) })
            }
        }
        SettingsGroup {
            listOf(Triple(translate("Donate"), "rufin-coffee-mug-symbolic", facts.donate),
                Triple(translate("Report an Issue"), "rufin-issue-symbolic", facts.issues),
                Triple(translate("Translate"), "rufin-translate-symbolic", facts.translate),
                Triple(translate("Chat on Matrix"), "rufin-chat-bubble-symbolic", facts.chat),
                Triple(translate("Website"), "rufin-globe-symbolic", facts.website)).forEach { (title, icon, link) ->
                SettingsRow(title, leading = { SettingsIcon(icon) }, trailing = { SettingsLeads(outside = true) },
                    modifier = Modifier.clickable {
                        try { context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(link))) } catch (_: ActivityNotFoundException) { }
                    })
            }
        }
        SettingsGroup {
            val credits = listOfNotNull("${translate("Developers")}\n${facts.developers.joinToString("\n") { it.substringBefore(" <") }}",
                facts.translators.trim().takeIf { it.isNotEmpty() }?.let { "${translate("Translators")}\n$it" }).joinToString("\n\n")
            SettingsRow(translate("Credits"), leading = { SettingsIcon("rufin-people-symbolic") },
                modifier = Modifier.clickable { details = translate("Credits") to credits })
            SettingsRow(translate("Legal"), leading = { SettingsIcon("rufin-copyright-symbolic") },
                trailing = { Text(facts.copyright, style = MaterialTheme.typography.bodyMedium, color = scheme.onSurfaceVariant) },
                modifier = Modifier.clickable { details = translate("Legal") to "${facts.copyright}\n\n${facts.license}" })
        }
    }
    BoxWithConstraints(Modifier.fillMaxSize()) {
        if (maxWidth >= 560.dp) Row(Modifier.fillMaxSize()) {
            app(Modifier.width(264.dp).fillMaxHeight().verticalScroll(rememberScrollState()).padding(horizontal = 16.dp, vertical = 16.dp))
            SettingsPage(Modifier.weight(1f), content = rows)
        } else SettingsPage {
            app(Modifier.fillMaxWidth())
            rows()
        }
    }
    details?.let { (title, text) ->
        AlertDialog(onDismissRequest = { details = null }, title = { Text(title) },
            text = { Text(text, Modifier.verticalScroll(rememberScrollState())) },
            confirmButton = { TextButton({ details = null }) { Text(translate("Close")) } })
    }
}
