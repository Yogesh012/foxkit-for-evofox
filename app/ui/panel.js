const { invoke } = window.__TAURI__.core;

// Music lighting reads no device and is checked often. Devices are checked less often, and not
// over HID at all while music lighting runs.
const MUSIC_MS = 1000;
const DEVICES_MS = 10000;
const LOW_BATTERY = 20;
const STYLES = [
  ["bars", "Bars"],
  ["pulse", "Pulse"],
  ["glow", "Glow"],
  ["ripple", "Ripple"],
  ["sparkle", "Sparkle"],
];
const BRIGHTNESS_LEVELS = [1, 2, 3, 4, 5];
const COLOURS = ["Red", "Green", "Blue", "Yellow", "Magenta", "Cyan", "White", "Colourful"];
const CONNECTIONS = {
  USB: "USB cable",
  "2.4 GHz receiver": "2.4 GHz receiver",
  Bluetooth: "Bluetooth",
};

const panel = document.getElementById("panel");
const toggle = document.getElementById("toggle");
const styles = document.getElementById("styles");
const look = document.getElementById("look");
const brightness = document.getElementById("brightness");
const colours = document.getElementById("colours");
const message = document.getElementById("message");
const login = document.getElementById("login");
let devices = null;
let music = null;
let busy = false;
let timers = [];

function element(tag, text, className) {
  const node = document.createElement(tag);
  if (text !== undefined) node.textContent = text;
  if (className) node.className = className;
  return node;
}

function isRunning() {
  return music?.bars != null && music.bars.phase !== "stopped";
}

function showMessage(text, problem = false) {
  message.hidden = !text;
  message.textContent = text ?? "";
  message.classList.toggle("problem", problem);
}

function batteryNode(battery) {
  const node = element("div", undefined, "battery");
  node.classList.toggle("charging", battery.charging);
  node.classList.toggle("low", !battery.charging && battery.percent <= LOW_BATTERY);
  const shape = element("div", undefined, "battery-shape");
  const level = element("div", undefined, "battery-level");
  level.style.width = `${Math.max(battery.percent, 4)}%`;
  shape.append(level);
  node.append(element("span", `${battery.percent}%`), shape);
  if (battery.charging) {
    const bolt = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    bolt.setAttribute("viewBox", "0 0 9 12");
    bolt.setAttribute("class", "bolt");
    bolt.setAttribute("aria-label", "Charging");
    const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
    path.setAttribute("d", "M5.5 0 0 7h3.5L3 12l6-7.5H5.2L5.5 0z");
    bolt.append(path);
    node.append(bolt);
  }
  return node;
}

function showDevice(id, device) {
  const row = document.getElementById(id);
  const text = row.querySelector(".device-state");
  const battery = row.querySelector(".battery");
  row.classList.toggle("absent", device.state !== "connected");
  text.classList.toggle("problem", device.state === "failed");
  if (device.state === "connected") {
    text.textContent = CONNECTIONS[device.connection] ?? device.connection;
    if (device.battery) {
      const fresh = batteryNode(device.battery);
      fresh.id = battery.id;
      battery.replaceWith(fresh);
      return;
    }
    text.textContent += ", battery not available";
  } else {
    text.textContent = device.state === "failed" ? device.message : "Not connected";
  }
  battery.hidden = true;
}

// Each group of buttons is made once and then only updated, so transitions and hover states
// carry on between refreshes.
function buttonGroup(container, entries, className, onPick, label) {
  container.replaceChildren(
    ...entries.map(([value, text], index) => {
      const swatch = className === "swatch";
      const button = element("button", swatch ? undefined : text, swatch ? `swatch swatch-${index}` : className);
      button.type = "button";
      button.dataset.value = String(value);
      if (label) button.setAttribute("aria-label", label(text));
      // Picks on press, as macOS controls feel. Keyboard presses arrive as a click.
      button.addEventListener("pointerdown", (event) => {
        if (event.button === 0 && !button.disabled) onPick(value);
      });
      button.addEventListener("click", (event) => {
        if (event.detail === 0) onPick(value);
      });
      return button;
    }),
  );
}

// Space between a colour circle and its ring.
const RING_GAP = 4;

function setPressed(container, pressed) {
  for (const button of container.querySelectorAll(":scope > button")) {
    button.setAttribute("aria-pressed", String(button.dataset.value === String(pressed)));
    button.disabled = busy;
  }
  moveIndicator(container);
}

// Slides the group's highlight to the selected button. It jumps instead when the group has
// just appeared, so it doesn't fly in from the corner.
function moveIndicator(container) {
  let indicator = container.querySelector(":scope > .indicator");
  if (!indicator) {
    indicator = element("span", undefined, "indicator");
    indicator.setAttribute("aria-hidden", "true");
    container.prepend(indicator);
  }
  const pressed = container.querySelector(':scope > button[aria-pressed="true"]');
  if (!pressed || container.offsetParent === null) {
    indicator.hidden = true;
    delete indicator.dataset.placed;
    return;
  }
  const instant = !indicator.dataset.placed;
  indicator.classList.toggle("instant", instant);
  indicator.hidden = false;
  const gap = container === colours ? RING_GAP : 0;
  indicator.style.width = `${pressed.offsetWidth + gap * 2}px`;
  indicator.style.height = `${pressed.offsetHeight + gap * 2}px`;
  indicator.style.transform = `translate(${pressed.offsetLeft - gap}px, ${pressed.offsetTop - gap}px)`;
  indicator.dataset.placed = "true";
  if (instant) {
    indicator.getBoundingClientRect();
    indicator.classList.remove("instant");
  }
}

buttonGroup(styles, STYLES, "chip", (style) => pickStyle(style));
buttonGroup(
  brightness,
  BRIGHTNESS_LEVELS.map((level) => [level, String(level)]),
  "segment",
  (value) => changeLook({ brightness: value }),
  (text) => `Brightness ${text}`,
);
buttonGroup(
  colours,
  COLOURS.map((name, index) => [index, name]),
  "swatch",
  (value) => changeLook({ colour: value }),
  (text) => text,
);

function showMusic() {
  if (!music) return;
  const running = isRunning();
  const current = running ? music.bars.look : null;
  toggle.textContent = running ? "Stop" : "Start";
  toggle.classList.toggle("on", running);
  toggle.disabled = busy || (!running && devices?.keyboard.state !== "connected");
  setPressed(styles, current?.style ?? music.style);
  look.hidden = !running;
  if (running) {
    setPressed(brightness, current.brightness);
    setPressed(colours, current.colour);
  } else {
    // Hidden groups jump into place next time they show.
    moveIndicator(brightness);
    moveIndicator(colours);
  }
}

async function refreshDevices() {
  try {
    devices = await invoke("panel_devices");
    showDevice("keyboard", devices.keyboard);
    showDevice("mouse", devices.mouse);
    showMusic();
  } catch (error) {
    showMessage(String(error), true);
  }
}

// Skipped while a change is on its way, so a slightly older reading doesn't undo the click.
// A reading that started before the latest change is out of date when it arrives.
let pending = 0;
let changes = 0;

async function refreshMusic() {
  if (pending > 0) return;
  const startedAfter = changes;
  try {
    const fresh = await invoke("panel_music");
    if (pending > 0 || changes !== startedAfter) return;
    music = fresh;
    showMusic();
  } catch (error) {
    showMessage(String(error), true);
  }
}

// Start and Stop wait for the keyboard, so the panel waits with them.
async function startOrStop(work) {
  busy = true;
  showMessage(null);
  showMusic();
  try {
    await work();
  } catch (error) {
    showMessage(String(error), true);
  } finally {
    busy = false;
    await refreshMusic();
    refreshDevices();
  }
}

// Other changes show straight away, and go to the app in the background.
async function changeNow(apply, send) {
  showMessage(null);
  apply();
  showMusic();
  changes += 1;
  pending += 1;
  try {
    await send();
  } catch (error) {
    showMessage(String(error), true);
  } finally {
    pending -= 1;
    await refreshMusic();
  }
}

function pickStyle(style) {
  if (isRunning()) return changeLook({ style });
  return changeNow(
    () => (music.style = style),
    () => invoke("set_style", { style }),
  );
}

function changeLook(change) {
  const look = { ...music.bars.look, ...change };
  return changeNow(
    () => (music.bars.look = look),
    () => invoke("change_bars", { look }),
  );
}

toggle.addEventListener("click", () =>
  startOrStop(async () => {
    if (!isRunning()) return invoke("start_bars");
    const report = await invoke("stop_bars");
    if (report?.problem) showMessage(report.problem, true);
  }),
);

login.addEventListener("change", async () => {
  try {
    login.checked = await invoke("set_launch_at_login", { enabled: login.checked });
  } catch (error) {
    login.checked = !login.checked;
    showMessage(String(error), true);
  }
});

document.getElementById("open").addEventListener("click", () => invoke("open_main"));
document.getElementById("quit").addEventListener("click", () => invoke("quit"));

// The window keeps its size to the content, which changes as devices come and go.
new ResizeObserver(() => {
  invoke("fit_panel", { height: Math.ceil(panel.getBoundingClientRect().height) });
}).observe(panel);

// Refreshes only while the panel is open.
function stopTimers() {
  timers.forEach(clearInterval);
  timers = [];
}

async function opened() {
  stopTimers();
  timers = [setInterval(refreshMusic, MUSIC_MS), setInterval(refreshDevices, DEVICES_MS)];
  await Promise.all([refreshMusic(), refreshDevices()]);
  login.checked = await invoke("launch_at_login");
}

window.addEventListener("focus", opened);
// In case the panel shows without taking focus.
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "visible") opened();
  else stopTimers();
});
window.addEventListener("blur", () => {
  stopTimers();
  showMessage(null);
});
if (document.hasFocus()) opened();
