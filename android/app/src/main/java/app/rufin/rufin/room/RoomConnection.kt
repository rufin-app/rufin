package app.rufin.rufin.room

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import app.rufin.rufin.app.RufinConnection
import app.rufin.rufin.core.translate
import app.rufin.rufin.shared.rufin_core.ActionException
import app.rufin.rufin.shared.rufin_core.RoomAction
import app.rufin.rufin.shared.rufin_core.RoomOwner
import app.rufin.rufin.shared.rufin_core.RoomStatus
import app.rufin.rufin.ui.userMessage
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.isActive
import kotlinx.coroutines.withContext

/** Listening together: the room this device listens in, as core tells it, and what the listener asks of it. */
@androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
internal class RoomConnection(private val model: RufinConnection) {
    var state by mutableStateOf<RoomStatus?>(null)
        private set
    /** Why creating or joining failed, for the sheet that asked. */
    var failure by mutableStateOf<String?>(null)
        private set
    var pending by mutableStateOf(false)
        private set
    /** The Rooms sheet, which the Home bar's and the queue bar's buttons open. */
    var sheet by mutableStateOf(false)
    /** An invite link that opened the app: the app asks whether to join its room. */
    var offered by mutableStateOf<String?>(null)
        private set
    private var bridge by mutableStateOf<RoomOwner?>(null)
    /** Core answers: before it does, nothing can be asked of a room. */
    val ready get() = bridge != null
    val room get() = state?.room

    suspend fun observe() {
        snapshotFlow { model.service }.collectLatest { service ->
            state = null
            if (service == null) return@collectLatest
            service.runtime.collectLatest { result ->
                val runtime = result?.getOrNull() ?: return@collectLatest
                val owner = runtime.room()
                bridge = owner
                val subscription = owner.changes()
                var told = owner.status().notice?.serial
                try {
                    while (currentCoroutineContext().isActive) {
                        val next = withContext(Dispatchers.Default) { subscription.next() }
                        val notice = next.notice
                        if (notice != null && notice.serial != told) model.showFeedback(notice.text)
                        told = notice?.serial
                        state = next
                    }
                } catch (cancelled: CancellationException) { throw cancelled }
                catch (error: Exception) { model.showFeedback(error) }
                finally {
                    bridge = null
                    state = null
                    subscription.destroy()
                    owner.destroy()
                }
            }
        }
    }

    fun action(action: RoomAction) {
        val owner = bridge ?: return
        model.runAction {
            pending = true
            failure = null
            try {
                // A room on this network, and a direct path to a member on it, need the local network.
                if (action is RoomAction.Create || action is RoomAction.Join) model.connectedService().requestLocalNetworkAccess()
                owner.execute(action).let { if (bridge === owner) state = it }
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (error: Exception) {
                val message = (error as? ActionException.Failed)?.v1 ?: error.userMessage()
                if (sheet) failure = message else model.showFeedback(message)
            } finally { pending = false }
        }
    }

    fun dismissFailure() { failure = null }

    /** What a listener reads of an invite before joining: that its room shares songs. */
    fun inviteNote(invite: String): String = bridge?.inviteNote(invite).orEmpty()

    fun offer(invite: String) {
        if (room != null) { model.showFeedback(translate("Leave this room first")); return }
        offered = invite
    }

    /** The name of the room an invite leads to; nothing for text that is no invite. */
    fun inviteRoom(invite: String): String = bridge?.inviteRoom(invite).orEmpty()

    /** The listener answered the offered invite, or it was no invite. */
    fun offerAnswered(invalid: Boolean = false) {
        if (invalid) model.showFeedback(translate("This is not a room invite"))
        offered = null
    }
}
