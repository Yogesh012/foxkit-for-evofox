const { invoke } = window.__TAURI__.core;

const REPORT_RATES = [
  [1000, "1000 Hz"],
  [500, "500 Hz"],
  [250, "250 Hz (not measured)"],
  [125, "125 Hz (not measured)"],
];
const BRIGHTNESS_MAX = 5;
const SPEED_MAX = 5;
const LEVELS = Array.from({ length: BRIGHTNESS_MAX }, (_, index) => [index + 1, String(index + 1)]);
const DIRECTIONS = [
  [0, "Left to right"],
  [1, "Right to left"],
];
const COLOURS = ["red", "green", "blue", "yellow", "magenta", "cyan", "white", "colourful"].map((name, index) => [index, `Preset ${index + 1} (${name})`]);
const DPI_MIN = 100;
const DPI_MAX = 12000;
const DPI_STEP = 100;
const DEBOUNCE_MIN = 1;
const DEBOUNCE_MAX = 20;
const MUSIC_STYLES = ["bars", "pulse", "glow", "ripple", "sparkle"];
const MUSIC_STYLE_OPTIONS = [
  [0, "Bars"],
  [1, "Pulse"],
  [2, "Glow"],
  [3, "Ripple"],
  [4, "Sparkle"],
];
const BARS_POLL_MS = 1000;
const KEYBOARD_WATCH_MS = 500;

const refreshButton = document.getElementById("refresh");
const keyboardPanel = document.getElementById("keyboard");
const mousePanel = document.getElementById("mouse");
let busy = false;
let mouseMessage = null;
let keyboardMessage = null;
let barsTimer = null;
let shownKeyboard = null;
let barsLook = null;
let barsInfo = null;

function element(tag, text, className) {
  const node = document.createElement(tag);
  if (text !== undefined) node.textContent = text;
  if (className) node.className = className;
  return node;
}

function details(rows) {
  const list = element("dl");
  for (const [name, value] of rows) {
    list.append(element("dt", name), element("dd", String(value)));
  }
  return list;
}

function batteryText(percent, charging) {
  return `${percent}%${charging ? ", charging" : ""}`;
}

function messageNode(message) {
  const node = element("p", message.text, `message ${message.className}`);
  node.setAttribute("role", "status");
  return node;
}

function selectControl(labelText, options, current, onChange) {
  const select = element("select");
  for (const [value, label] of options) {
    const option = element("option", label);
    option.value = value;
    option.selected = value === current;
    select.append(option);
  }
  select.addEventListener("change", () => onChange(Number(select.value)));
  return field(labelText, select);
}

function keyboardDetails(status) {
  shownKeyboard = JSON.stringify(status);
  const nodes = [
    details([
      ["Connection", status.connection],
      ["Battery", batteryText(status.battery, status.charging)],
      ["Firmware", status.firmware],
    ]),
    element("h3", "Lighting"),
  ];
  const lighting = {
    effect: status.effectNumber,
    // Custom and Music Rhythm can report values outside the ranges a command accepts.
    brightness: Math.min(status.brightness, BRIGHTNESS_MAX),
    speed: Math.min(status.speed, SPEED_MAX),
    direction: Math.min(status.direction, DIRECTIONS.length - 1),
    colour: Math.min(status.colour, COLOURS.length - 1),
  };
  const set = (name, label) => (value) => {
    changeKeyboard({ kind: "lighting", ...lighting, [name]: value }, `${label} changed.`);
  };
  const effects = status.effectNames.map((name, index) => [index + 1, name]);
  const effectControl = selectControl("Effect", effects, status.effectNumber, set("effect", "Effect"));
  const others = [
    selectControl("Brightness", LEVELS, status.brightness, set("brightness", "Brightness")),
    selectControl("Speed", LEVELS, status.speed, set("speed", "Speed")),
    selectControl("Direction", DIRECTIONS, status.direction, set("direction", "Direction")),
    selectControl("Colour", COLOURS, status.colour, set("colour", "Colour")),
  ];
  const off = element("button", "Turn lights off");
  off.type = "button";
  off.addEventListener("click", () => changeKeyboard({ kind: "lightsOff" }, "Lights off. Change any setting to turn them back on."));
  const controls = element("div", undefined, "controls");
  controls.append(effectControl, ...others, off);
  nodes.push(controls);
  const startBars = element("button", "Start audio bars");
  startBars.type = "button";
  startBars.addEventListener("click", beginBars);
  nodes.push(element("h3", "Audio bars"), startBars, element("p", "Ctrl+Option+M starts and stops them while this app is open.", "note"));
  if (status.effectNumber === null) {
    startBars.disabled = true;
    startBars.dataset.locked = "true";
    // These controls resend the whole lighting command, which would leave this effect.
    for (const label of others) {
      const select = label.querySelector("select");
      select.disabled = true;
      select.dataset.locked = "true";
    }
    effectControl.querySelector("select").selectedIndex = -1;
    nodes.push(element("p", `The keyboard is on ${status.effect}. Pick an effect to change the other settings.`, "note"));
  }
  if (keyboardMessage) nodes.push(messageNode(keyboardMessage));
  return nodes;
}

function barsSummary(report) {
  const summary = `${report.frames} frames in ${Math.round(report.seconds)} s, ${report.late} late, ${report.failedPages} pages failed.`;
  const stopped = report.endedByKeyboard ? "Audio bars stopped by the effect shortcut." : "Audio bars stopped.";
  return report.frames ? `${stopped} ${summary}` : stopped;
}

function barsRows(report) {
  const rows = [["State", report.phase === "starting" ? "Starting" : "Running"]];
  if (report.output) rows.push(["Output", report.output]);
  rows.push(
    ["Running for", `${Math.floor(report.seconds / 60)} min ${Math.floor(report.seconds % 60)} s`],
    ["Frames", report.frames],
    ["Late", report.late],
    ["Pages failed", report.failedPages],
    ["Slowest frame", `${Math.round(report.slowestMs)} ms`],
  );
  if (report.keyboardEffect) rows.push(["Keyboard reports", report.keyboardEffect]);
  return rows;
}

async function changeBars(name, value) {
  barsLook = { ...barsLook, [name]: value };
  try {
    await invoke("change_bars", { look: barsLook });
  } catch (error) {
    keyboardMessage = { text: String(error), className: "problem" };
  }
}

function showBars(report) {
  const info = details(barsRows(report));
  const sameLook = barsLook && ["brightness", "colour", "direction", "style"].every((name) => barsLook[name] === report.look[name]);
  // Only the numbers change most of the time, and redrawing the menus would close an open one.
  if (sameLook && barsInfo?.isConnected) {
    barsInfo.replaceWith(info);
    barsInfo = info;
    return;
  }
  barsLook = report.look;
  barsInfo = info;
  const controls = element("div", undefined, "controls");
  const set = (name) => (value) => changeBars(name, value);
  controls.append(
    selectControl("Style", MUSIC_STYLE_OPTIONS, MUSIC_STYLES.indexOf(report.look.style), (index) => changeBars("style", MUSIC_STYLES[index])),
    selectControl("Brightness", LEVELS, report.look.brightness, set("brightness")),
    selectControl("Direction", DIRECTIONS, report.look.direction, set("direction")),
    selectControl("Colour", COLOURS, report.look.colour, set("colour")),
  );
  const stop = element("button", "Stop audio bars");
  stop.type = "button";
  stop.addEventListener("click", endBars);
  const note = element("p", "These apply to the bars only. Your normal lighting comes back when they stop.", "note");
  keyboardPanel.replaceChildren(info, element("h3", "Audio bars"), controls, note, stop);
}

function setBarsPolling(active) {
  clearInterval(barsTimer);
  barsTimer = active ? setInterval(pollBars, BARS_POLL_MS) : null;
  // These numbers change every second, too often to read out.
  keyboardPanel.setAttribute("aria-live", active ? "off" : "polite");
}

async function finishBars(report) {
  setBarsPolling(false);
  keyboardMessage = report.problem
    ? { text: report.problem, className: "problem" }
    : { text: barsSummary(report), className: "note" };
  await refresh();
}

async function pollBars() {
  const report = await invoke("bars_report");
  if (barsTimer === null) return;
  if (report === null) {
    setBarsPolling(false);
    await refresh();
  } else if (report.phase === "stopped") {
    await finishBars(report);
  } else {
    showBars(report);
  }
}

async function beginBars() {
  setBusy(true);
  keyboardMessage = null;
  try {
    await invoke("start_bars");
    setBarsPolling(true);
    await pollBars();
  } catch (error) {
    keyboardMessage = { text: String(error), className: "problem" };
    await refresh();
  } finally {
    setBusy(false);
  }
}

async function endBars() {
  setBarsPolling(false);
  setBusy(true);
  try {
    const report = await invoke("stop_bars");
    if (report) await finishBars(report);
  } finally {
    setBusy(false);
  }
}

function setBusy(value) {
  busy = value;
  refreshButton.disabled = value;
  for (const control of document.querySelectorAll("main button, main input, main select")) {
    control.disabled = value || control.dataset.locked === "true";
  }
}

async function changeDevice(command, change, done, setMessage, showStatus) {
  setBusy(true);
  try {
    const status = await invoke(command, { change });
    setMessage({ text: done, className: "note" });
    showStatus(status);
  } catch (error) {
    setMessage({ text: String(error), className: "problem" });
    await refresh();
  } finally {
    setBusy(false);
  }
}

function changeMouse(change, done) {
  return changeDevice("change_mouse", change, done, (message) => (mouseMessage = message), showMouse);
}

function changeKeyboard(change, done) {
  return changeDevice("change_keyboard", change, done, (message) => (keyboardMessage = message), showKeyboard);
}

function field(labelText, control) {
  const label = element("label", labelText);
  label.append(control);
  return label;
}

function applyForm(labelText, attributes, value, makeChange) {
  const form = element("form");
  const input = element("input");
  Object.assign(input, { type: "number", required: true, value, ...attributes });
  const apply = element("button", "Apply");
  apply.type = "submit";
  form.append(field(labelText, input), apply);
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    if (!busy) makeChange(Number(input.value));
  });
  return form;
}

function reportRateControl(current) {
  const select = element("select");
  for (const [hz, label] of REPORT_RATES) {
    const option = element("option", label);
    option.value = hz;
    option.selected = hz === current;
    select.append(option);
  }
  select.addEventListener("change", () => {
    const hz = Number(select.value);
    changeMouse({ kind: "reportRate", hz }, `Report rate set to ${hz} Hz.`);
  });
  return field("Report rate", select);
}

function levelButtons(levels) {
  const row = element("div", undefined, "levels");
  row.setAttribute("role", "group");
  row.setAttribute("aria-label", "Active DPI level");
  for (const level of levels) {
    const label = level.dpi === null ? `Value ${level.value}` : String(level.dpi);
    const button = element("button", label, level.active ? "active" : undefined);
    button.type = "button";
    button.setAttribute("aria-pressed", String(level.active));
    button.addEventListener("click", () => {
      if (!level.active) changeMouse({ kind: "activeLevel", level: level.number }, `Switched to level ${level.number}.`);
    });
    row.append(button);
  }
  return row;
}

function levelEditor(levels) {
  const select = element("select");
  for (const level of levels) {
    const option = element("option", `Level ${level.number}`);
    option.value = level.number;
    option.selected = level.active;
    select.append(option);
  }
  const current = () => levels.find((level) => level.number === Number(select.value));
  const form = applyForm("DPI", { min: DPI_MIN, max: DPI_MAX, step: DPI_STEP }, current().dpi ?? "", (dpi) => {
    const level = current();
    changeMouse({ kind: "levelDpi", level: level.number, dpi }, `Level ${level.number} set to ${dpi} DPI.`);
  });
  select.addEventListener("change", () => {
    form.querySelector("input").value = current().dpi ?? "";
  });
  form.prepend(field("Edit", select));
  return form;
}

function mouseDetails(status) {
  const nodes = [
    details([
      ["Battery", batteryText(status.batteryPercent, status.charging)],
      ["Firmware", status.firmware],
    ]),
    element("h3", "Performance"),
    reportRateControl(status.reportRateHz),
    element("h3", "DPI level"),
    levelButtons(status.levels),
    levelEditor(status.levels),
    element("h3", "Click debounce"),
    applyForm("Short debounce", { min: DEBOUNCE_MIN, max: DEBOUNCE_MAX, step: 1 }, status.shortDebounce, (value) =>
      changeMouse({ kind: "shortDebounce", value }, `Short debounce set to ${value}.`),
    ),
    element("p", "Lower values may register fast clicks sooner, but can cause accidental double clicks. The mouse stores this value; its effect on clicks hasn't been measured.", "note"),
  ];
  if (mouseMessage) nodes.push(messageNode(mouseMessage));
  return nodes;
}

function showMouse(status) {
  mousePanel.replaceChildren(...mouseDetails(status));
}

function showKeyboard(status) {
  keyboardPanel.replaceChildren(...keyboardDetails(status));
}

// Picks up what happened outside the window: the Mac-wide shortcut, and changes made on the
// keyboard itself.
async function watchKeyboard() {
  if (busy || barsTimer !== null) return;
  const problem = await invoke("shortcut_problem");
  if (problem !== null) {
    keyboardMessage = { text: problem, className: "problem" };
    await refresh();
  }
  const report = await invoke("bars_report");
  if (busy || barsTimer !== null) return;
  if (report !== null && report.phase !== "stopped") {
    keyboardMessage = null;
    setBarsPolling(true);
    await pollBars();
    return;
  }
  const status = await invoke("keyboard_report");
  if (status === null || busy || barsTimer !== null) return;
  if (JSON.stringify(status) !== shownKeyboard) showKeyboard(status);
}

function show(panel, reading, showStatus) {
  switch (reading.state) {
    case "connected":
      panel.replaceChildren(...showStatus(reading.status));
      break;
    case "noSettings":
      panel.replaceChildren(element("p", `Connected as ${reading.name}, but settings aren't available over this connection. Use the USB cable.`, "note"));
      break;
    case "notFound":
      panel.replaceChildren(element("p", "Not connected.", "note"));
      break;
    case "busy":
      if (barsTimer === null) setBarsPolling(true);
      break;
    default:
      panel.replaceChildren(element("p", reading.message, "problem"));
  }
}

async function refresh() {
  refreshButton.disabled = true;
  try {
    const devices = await invoke("read_devices");
    show(keyboardPanel, devices.keyboard, keyboardDetails);
    show(mousePanel, devices.mouse, mouseDetails);
  } catch (error) {
    const problem = String(error);
    keyboardPanel.replaceChildren(element("p", problem, "problem"));
    mousePanel.replaceChildren(element("p", problem, "problem"));
  } finally {
    refreshButton.disabled = false;
  }
}

refreshButton.addEventListener("click", () => {
  mouseMessage = null;
  keyboardMessage = null;
  refresh();
});
refresh();
setInterval(watchKeyboard, KEYBOARD_WATCH_MS);
