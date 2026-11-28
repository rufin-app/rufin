package app.rufin.rufin.room

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import app.rufin.rufin.app.RufinConnection
import app.rufin.rufin.core.translate
import app.rufin.rufin.player.PlayerIconButton
import app.rufin.rufin.settings.settingsTextFieldColors
import app.rufin.rufin.shared.rufin_core.RoomAction
import app.rufin.rufin.shared.rufin_core.RoomStatus
import app.rufin.rufin.shared.rufin_core.RoomView
import app.rufin.rufin.ui.RufinIcon
import kotlinx.coroutines.delay

/** A listener's mark: the first letter of the name, on the accent. */
@Composable
internal fun RoomAvatar(name: String, size: Dp = 24.dp, modifier: Modifier = Modifier) {
    Box(modifier.size(size).background(MaterialTheme.colorScheme.primary, CircleShape), contentAlignment = Alignment.Center) {
        Text(name.trim().take(1).uppercase(), color = MaterialTheme.colorScheme.onPrimary, fontSize = (size.value * .46f).sp,
            lineHeight = (size.value * .46f).sp, fontWeight = FontWeight.SemiBold)
    }
}

/** The Home bar's Rooms button: in a room it is in the accent and counts who listens. */
@Composable
internal fun RoomsButton(rooms: RoomConnection) {
    val listening = rooms.room?.members?.size
    if (listening == null) PlayerIconButton("rufin-people-symbolic", "Rooms", { rooms.sheet = true })
    else TextButton({ rooms.sheet = true }, contentPadding = PaddingValues(horizontal = 10.dp)) {
        RufinIcon("rufin-people-symbolic", translate("Rooms"), Modifier.size(24.dp), MaterialTheme.colorScheme.primary)
        Text(listening.toString(), Modifier.padding(start = 4.dp), fontWeight = FontWeight.SemiBold)
    }
}

/** An invite link opened the app: asks whether to join its room, and under which name. */
@Composable
internal fun RoomInviteDialog(rooms: RoomConnection) {
    val invite = rooms.offered ?: return
    // Core reads the invite: until it answers, the question waits.
    if (!rooms.ready) return
    val room = remember(invite) { rooms.inviteRoom(invite) }
    if (room.isEmpty()) {
        LaunchedEffect(invite) { rooms.offerAnswered(invalid = true) }
        return
    }
    val known = rooms.state?.yourName.orEmpty()
    var name by rememberSaveable(known) { mutableStateOf(known) }
    AlertDialog(onDismissRequest = { rooms.offerAnswered() },
        title = { Text(translate("Join {room}?").replace("{room}", room)) },
        text = {
            OutlinedTextField(name, { name = it.take(40) }, Modifier.fillMaxWidth(), singleLine = true,
                label = { Text(translate("Your name")) }, colors = settingsTextFieldColors())
        },
        confirmButton = { TextButton({ rooms.offerAnswered(); rooms.action(RoomAction.Join(invite, name)) }) { Text(translate("Join")) } },
        dismissButton = { TextButton({ rooms.offerAnswered() }) { Text(translate("Cancel")) } })
}

/** The Rooms sheet: creating or joining a room, and who is in the room this device listens in. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun RoomsSheet(model: RufinConnection, onRelay: () -> Unit, onDismiss: () -> Unit) {
    val rooms = model.room
    val status = rooms.state
    val room = status?.room
    val close = { rooms.dismissFailure(); onDismiss() }
    ModalBottomSheet(onDismissRequest = close, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
        Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(horizontal = 20.dp).padding(bottom = 24.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp)) {
            if (room == null) RoomsStart(rooms, status, onRelay) else RoomMembers(rooms, room, close)
        }
    }
}

@Composable
private fun RoomsStart(rooms: RoomConnection, status: RoomStatus?, onRelay: () -> Unit) {
    val caption = MaterialTheme.colorScheme.onSurfaceVariant
    var name by rememberSaveable(status?.yourName) { mutableStateOf(status?.yourName.orEmpty()) }
    var invite by rememberSaveable { mutableStateOf("") }
    Text(translate("Rooms"), Modifier.fillMaxWidth(), textAlign = TextAlign.Center, style = MaterialTheme.typography.titleLarge)
    OutlinedTextField(name, { name = it.take(40) }, Modifier.fillMaxWidth(), singleLine = true,
        label = { Text(translate("Your name")) }, colors = settingsTextFieldColors())
    // A join under way is given up from where it was started.
    val joining = status?.joining == true
    Button({ rooms.action(RoomAction.Create(name)) }, Modifier.fillMaxWidth(), enabled = rooms.ready && !rooms.pending) { Text(translate("Create room")) }
    Column(Modifier.fillMaxWidth(), horizontalAlignment = Alignment.CenterHorizontally) {
        Text(translate("Shares your queue. Your music keeps playing."), textAlign = TextAlign.Center,
            style = MaterialTheme.typography.bodySmall, color = caption)
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(status?.reach.orEmpty(), Modifier.weight(1f, fill = false), maxLines = 1, overflow = TextOverflow.Ellipsis,
                style = MaterialTheme.typography.bodySmall, color = caption)
            TextButton(onRelay) { Text(translate("Change")) }
        }
    }
    Text(translate("Join a room"), style = MaterialTheme.typography.titleSmall, color = MaterialTheme.colorScheme.primary)
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        OutlinedTextField(invite, { invite = it }, Modifier.weight(1f), enabled = !joining, singleLine = true,
            label = { Text(translate("Invite link")) }, colors = settingsTextFieldColors())
        if (joining) FilledTonalButton({ rooms.action(RoomAction.Leave) }) { Text(translate("Cancel")) }
        else FilledTonalButton({ rooms.action(RoomAction.Join(invite.trim(), name)) },
            enabled = invite.isNotBlank() && rooms.ready && !rooms.pending) { Text(translate("Join")) }
    }
    val note = remember(invite, rooms.ready) { rooms.inviteNote(invite.trim()) }
    if (note.isNotEmpty()) Text(note, style = MaterialTheme.typography.bodySmall, color = caption)
    if (rooms.pending) LinearProgressIndicator(Modifier.fillMaxWidth())
    rooms.failure?.let { Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall) }
}

@Composable
private fun RoomMembers(rooms: RoomConnection, room: RoomView, close: () -> Unit) {
    val context = LocalContext.current
    val caption = MaterialTheme.colorScheme.onSurfaceVariant
    var copied by remember { mutableStateOf(false) }
    LaunchedEffect(copied) { if (copied) { delay(1500); copied = false } }
    var ending by remember { mutableStateOf(false) }
    if (ending) AlertDialog(onDismissRequest = { ending = false },
        title = { Text(translate("End the room?")) },
        text = { Text(translate("Everyone stops listening together")) },
        confirmButton = { TextButton({ ending = false; close(); rooms.action(RoomAction.Leave) },
            colors = ButtonDefaults.textButtonColors(contentColor = MaterialTheme.colorScheme.error)) { Text(translate("End room")) } },
        dismissButton = { TextButton({ ending = false }) { Text(translate("Cancel")) } })
    Column(Modifier.fillMaxWidth(), horizontalAlignment = Alignment.CenterHorizontally) {
        Text(room.name, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.titleLarge)
        Text(translate("{count} listening").replace("{count}", room.members.size.toString()),
            style = MaterialTheme.typography.bodySmall, color = caption)
    }
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Button({
            context.startActivity(Intent.createChooser(Intent(Intent.ACTION_SEND).setType("text/plain")
                .putExtra(Intent.EXTRA_TEXT, room.invite), translate("Share invite")))
        }, Modifier.weight(1f)) {
            RufinIcon("rufin-share-symbolic", null, Modifier.size(18.dp))
            Text(translate("Share invite"), Modifier.padding(start = 8.dp), maxLines = 1)
        }
        FilledTonalButton({
            (context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager)
                .setPrimaryClip(ClipData.newPlainText(translate("Invite link"), room.invite))
            copied = true
        }, Modifier.weight(1f)) {
            RufinIcon(if (copied) "rufin-object-select-symbolic" else "rufin-edit-copy-symbolic", null, Modifier.size(18.dp))
            Text(translate("Copy link"), Modifier.padding(start = 8.dp), maxLines = 1)
        }
    }
    Column(Modifier.fillMaxWidth().clip(RoundedCornerShape(18.dp)), verticalArrangement = Arrangement.spacedBy(3.dp)) {
        room.members.forEach { member ->
            Surface(color = MaterialTheme.colorScheme.surfaceContainer) {
                Row(Modifier.fillMaxWidth().heightIn(min = 56.dp).padding(start = 12.dp, end = 8.dp),
                    verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    RoomAvatar(member.name, 32.dp)
                    Column(Modifier.weight(1f)) {
                        Text(member.name, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodyLarge)
                        Text(if (member.sittingOut) translate("Sitting this one out") else translate("Listening"),
                            style = MaterialTheme.typography.bodySmall, color = caption)
                    }
                    (if (member.host) translate("Host") else if (member.you) translate("You") else null)?.let {
                        Text(it, Modifier.padding(end = 4.dp), style = MaterialTheme.typography.labelMedium, color = caption)
                    }
                    if (room.hosting && !member.host) IconButton({ rooms.action(RoomAction.Remove(member.id)) }) {
                        RufinIcon("rufin-list-remove-symbolic", translate("Remove from room"), Modifier.size(20.dp))
                    }
                }
            }
        }
    }
    // Everyone sees whether the room shares songs; the host decides it.
    Surface(Modifier.fillMaxWidth().clip(RoundedCornerShape(18.dp)), color = MaterialTheme.colorScheme.surfaceContainer) {
        Row(Modifier.toggleable(room.sharing, enabled = room.hosting, role = Role.Switch) { rooms.action(RoomAction.Share(it)) }
            .heightIn(min = 56.dp).padding(horizontal = 12.dp, vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text(translate("Share songs"), style = MaterialTheme.typography.bodyLarge)
                Text(translate("Members who have a song play it for listeners who don't"), style = MaterialTheme.typography.bodySmall, color = caption)
            }
            Switch(room.sharing, null, enabled = room.hosting)
        }
    }
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.Center, verticalAlignment = Alignment.CenterVertically) {
        Text(room.reach, Modifier.weight(1f, fill = false), maxLines = 1, overflow = TextOverflow.Ellipsis,
            style = MaterialTheme.typography.bodySmall, color = caption)
        if (room.relayOffered) TextButton({ rooms.action(RoomAction.UseRelay) }) { Text(translate("Use for my rooms")) }
    }
    TextButton({ if (room.hosting) ending = true else { close(); rooms.action(RoomAction.Leave) } }, Modifier.fillMaxWidth(),
        colors = ButtonDefaults.textButtonColors(contentColor = MaterialTheme.colorScheme.error)) {
        RufinIcon("rufin-application-exit-symbolic", null, Modifier.size(18.dp))
        Text(if (room.hosting) translate("End room") else translate("Leave room"), Modifier.padding(start = 8.dp))
    }
}
