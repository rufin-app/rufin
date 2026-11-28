// Rooms: the top bar's button and popover, the room's notices, and the room's part of the queue.
import { api, state } from "./connection.js";
import { showModes } from "./player.js";
import { openIntegrationPage } from "./integrations.js";
import { $, el, icon, notice, run, tr } from "./ui.js";

let status = { room: null, notice: null, your_name: "", reach: "", joining: false };
let told;

const act = (action) => api("/room", "POST", action);

// One queue row of a room: how many members have its song, where not all do, and who added it.
function decorate(row, track) {
  row.querySelector(".queue-room")?.remove();
  const room = status.room;
  // A song this library has no copy of: nothing to hear, unless the room shares songs.
  const lacking = track.uri.startsWith("rufin-room:");
  row.classList.toggle("lacking", lacking && !room?.sharing);
  const artist = row.querySelector(".queue-artist");
  if (lacking && artist) artist.textContent = tr("Not in your library");
  if (!room) return;
  const position = Number(row.dataset.position);
  const members = room.members.length;
  const count = room.counts[position] ?? members;
  const by = room.added_by[position] || "";
  const cell = el("span", "queue-room");
  if (count < members) {
    const have = el("span", "room-have", `${count}/${members}`);
    have.title = tr("{count} of {members} can play this", { count, members });
    cell.append(have);
  }
  if (by) {
    const avatar = el("span", "room-avatar", by.trim().charAt(0).toUpperCase());
    avatar.title = tr("Added by {name}", { name: by });
    cell.append(avatar);
  }
  row.querySelector(".queue-track")?.after(cell);
}

function show() {
  const room = status.room;
  $("rooms-start").hidden = !!room;
  $("rooms-room").hidden = !room;
  $("rooms-count").hidden = !room;
  $("rooms-button").classList.toggle("in-room", !!room);
  $("rooms-button").title = room ? room.name : tr("Rooms");
  $("app").toggleAttribute("data-room", !!room);
  // Only the host clears a room's queue.
  $("clear-queue").disabled = !!room && !room.hosting;
  state.room = room;
  showModes();
  $("queue-room").hidden = !room;
  // What the listener just played here can be taken back while the room still can.
  $("queue-room-undo").hidden = !room?.undo;
  if (!room) {
    if (!$("rooms-name").value) $("rooms-name").value = status.your_name;
    $("rooms-reach").textContent = status.reach;
    // A join under way is given up from where it was started.
    const joining = status.joining;
    $("rooms-join").textContent = joining ? tr("Cancel") : tr("Join");
    $("rooms-join").disabled = !joining && !$("rooms-invite").value.trim();
    $("rooms-invite").disabled = joining;
    $("rooms-create").disabled = joining;
    return;
  }
  $("rooms-invite").value = "";
  // Everyone sees whether the room shares songs; the host decides it.
  $("rooms-share").disabled = !room.hosting;
  $("rooms-share").checked = room.sharing;
  $("rooms-room-reach").textContent = room.reach;
  $("rooms-use-relay").hidden = !room.relay_offered;
  $("rooms-count").textContent = room.members.length;
  $("rooms-failure").hidden = true;
  $("rooms-room-name").textContent = room.name;
  $("rooms-room-count").textContent = tr("{count} listening", { count: room.members.length });
  $("queue-room-name").textContent = room.name;
  $("queue-room-songs").textContent = tr("You have {have} of {songs} songs", { have: room.have, songs: room.songs });
  $("rooms-leave").lastElementChild.textContent = room.hosting ? tr("End room") : tr("Leave room");
  $("rooms-members").replaceChildren(
    ...room.members.map((member) => {
      const row = el("li", "rooms-member");
      const text = el("span", "rooms-member-text");
      text.append(
        el("strong", "", member.name),
        el("small", "", member.sitting_out ? tr("Sitting this one out") : tr("Listening")),
      );
      row.append(el("span", "room-avatar", member.name.trim().charAt(0).toUpperCase()), text);
      if (member.host) row.append(el("span", "rooms-tag", tr("Host")));
      else if (member.you) row.append(el("span", "rooms-tag", tr("You")));
      if (room.hosting && !member.host) {
        const remove = el("button", "icon-button");
        remove.type = "button";
        remove.title = tr("Remove from room");
        remove.setAttribute("aria-label", remove.title);
        remove.append(icon("rufin-list-remove-symbolic"));
        remove.addEventListener("click", () => run(() => act({ action: "remove", member: member.id })));
        row.append(remove);
      }
      return row;
    }),
  );
}

function take(value) {
  const first = told === undefined;
  status = value;
  show();
  for (const row of document.querySelectorAll("#queue .queue-row")) {
    const uri = row.dataset.uri;
    if (uri !== undefined) decorate(row, { uri });
  }
  const said = status.notice;
  const serial = said?.serial ?? 0;
  if (!first && said && serial !== told) notice(said.text);
  told = serial;
}

// What an action answers shows in the popover while it is open.
async function asked(action) {
  const failure = $("rooms-failure");
  failure.hidden = true;
  try {
    take(await act(action));
  } catch (error) {
    failure.textContent = error.message;
    failure.hidden = false;
  }
}

function init() {
  $("rooms-create").addEventListener("click", () => asked({ action: "create", name: $("rooms-name").value }));
  const invite = $("rooms-invite");
  const join = () => {
    if (status.joining) return asked({ action: "leave" });
    const link = invite.value.trim();
    if (link) asked({ action: "join", invite: link, name: $("rooms-name").value });
  };
  invite.addEventListener("input", () => {
    const link = invite.value.trim();
    $("rooms-join").disabled = !link && !status.joining;
    // What the invite's room does with songs is read before joining.
    run(async () => {
      const { note } = link ? await api("/room/note", "POST", { invite: link }) : { note: "" };
      if (invite.value.trim() !== link) return;
      $("rooms-invite-note").textContent = note;
      $("rooms-invite-note").hidden = !note;
    });
  });
  invite.addEventListener("keydown", (event) => { if (event.key === "Enter") join(); });
  $("rooms-join").addEventListener("click", join);
  $("rooms-reach-change").addEventListener("click", () => {
    $("rooms-popover").hidePopover();
    openIntegrationPage("iroh_relay");
  });
  $("rooms-use-relay").addEventListener("click", () => asked({ action: "use_relay" }));
  $("rooms-share").addEventListener("change", (event) => asked({ action: "share", songs: event.target.checked }));
  $("rooms-copy").addEventListener("click", () =>
    run(async () => {
      await navigator.clipboard.writeText(status.room?.invite || "");
      notice(tr("Copied"));
    }),
  );
  $("queue-room-undo").addEventListener("click", () => run(() => act({ action: "undo" })));
  $("rooms-leave").addEventListener("click", () => {
    if (status.room?.hosting && !confirm(`${tr("End the room?")}\n${tr("Everyone stops listening together")}`)) return;
    $("rooms-popover").hidePopover();
    run(() => act({ action: "leave" }));
  });
  run(async () => take(await api("/room")));
  const events = new EventSource("/api/room/events");
  events.addEventListener("room", (event) => take(JSON.parse(event.data)));
}

export { init, decorate };
