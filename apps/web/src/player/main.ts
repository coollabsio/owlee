import rrwebPlayer from "rrweb-player";
import "rrweb-player/dist/style.css";
import "./player.css";
import type { FromPlayer, RrwebEvent, ToPlayer } from "@/lib/replay";

/* Runs inside /replay-player.html, framed by the replay page. The server gives this document a CSP with
   connect-src 'none': every event arrives by postMessage from the parent. rrweb rebuilds the recorded DOM in
   an inner iframe with sandbox="allow-same-origin" and no allow-scripts. Never set UNSAFE_replayCanvas. */

const CONTROLLER_HEIGHT = 80;
const root = document.getElementById("player")!;
// Svelte methods that the published types drop, because svelte itself is bundled, not a dependency.
type Player = rrwebPlayer & { $destroy(): void; $set(props: { width: number; height: number }): void };

let player: Player | null = null;
let state = { timeMs: 0, playing: false };
let lastPostedTime = -Infinity;

function post(message: FromPlayer) {
  window.parent.postMessage(message, location.origin);
}

function postState(timeMs: number, playing: boolean, force = false) {
  state = { timeMs, playing };
  if (!force && Math.abs(timeMs - lastPostedTime) < 250) return;
  lastPostedTime = timeMs;
  post({ type: "state", ...state });
}

function size() {
  return { width: window.innerWidth, height: Math.max(120, window.innerHeight - CONTROLLER_HEIGHT) };
}

function load(events: RrwebEvent[]) {
  player?.$destroy();
  player = null;
  root.textContent = "";
  if (events.length < 2) {
    root.textContent = "Nothing to play yet.";
    return;
  }
  player = new rrwebPlayer({
    target: root,
    props: {
      events: events as never,
      ...size(),
      autoPlay: false,
      maxScale: 0,
      mouseTail: false,
      UNSAFE_replayCanvas: false,
    },
  }) as Player;
  player.addEventListener("ui-update-current-time", (detail) => postState((detail as { payload: number }).payload, state.playing));
  player.addEventListener("ui-update-player-state", (detail) =>
    postState(state.timeMs, (detail as { payload: string }).payload === "playing", true),
  );
}

function receive(message: ToPlayer) {
  switch (message.type) {
    case "load":
      load(message.events);
      break;
    case "append":
      for (const event of message.events) player?.addEvent(event as never);
      break;
    case "seek":
      player?.goto(Math.max(0, message.timeMs), false);
      postState(Math.max(0, message.timeMs), false, true);
      break;
  }
}

// Opened on its own, the player has no parent to trust and shows nothing.
if (window.parent !== window) {
  window.addEventListener("message", (event) => {
    if (event.source !== window.parent || event.origin !== location.origin) return;
    receive(event.data as ToPlayer);
  });
  window.addEventListener("resize", () => {
    if (!player) return;
    player.$set(size());
    player.triggerResize();
  });
  post({ type: "ready" });
}
