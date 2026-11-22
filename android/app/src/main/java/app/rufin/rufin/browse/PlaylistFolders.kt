package app.rufin.rufin.browse

import androidx.compose.material3.*
import androidx.compose.runtime.*
import app.rufin.rufin.app.RufinConnection
import app.rufin.rufin.core.translate
import app.rufin.rufin.shared.library.PlaylistFolderMember
import app.rufin.rufin.shared.library.PlaylistFolderRow
import app.rufin.rufin.shared.rufin_core.addToPlaylistFolder
import app.rufin.rufin.shared.rufin_core.createPlaylistFolder
import app.rufin.rufin.shared.rufin_core.renamePlaylistFolder

/** Names a new folder, or renames `folder`; a new one holds `members`, and `onCreated` runs
 *  before the dialog closes. */
@Composable
internal fun PlaylistFolderNameDialog(
    model: RufinConnection, onDismiss: () -> Unit,
    folder: PlaylistFolderRow? = null,
    members: List<PlaylistFolderMember> = emptyList(),
    onCreated: () -> Unit = {},
) {
    var name by remember(folder?.folderKey) { mutableStateOf(folder?.name.orEmpty()) }
    AlertDialog(onDismissRequest = onDismiss, title = { Text(translate(if (folder == null) "New folder" else "Rename folder")) },
        text = { OutlinedTextField(name, { name = it }, label = { Text(translate("Folder name")) }, singleLine = true) },
        confirmButton = { TextButton({ model.runAction {
            val source = model.sourceOwner ?: return@runAction
            if (folder == null) {
                val key = createPlaylistFolder(source, name.trim())
                if (members.isNotEmpty()) addToPlaylistFolder(source, members, key)
                onCreated()
            } else renamePlaylistFolder(source, folder.folderKey, name.trim())
            onDismiss()
        } }, enabled = name.isNotBlank() && model.pendingActions == 0) {
            Text(translate(if (folder == null) "Create" else "Save"))
        } }, dismissButton = { TextButton(onDismiss) { Text(translate("Cancel")) } })
}
