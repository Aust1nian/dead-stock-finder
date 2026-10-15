// Idle Stock Finder's window: connect, set up, the report, settings, help.

import { api, on, type AppView, type Outcome, type ProgressEvent, type SelfTestReport, type Settings } from "./api";
import { clear, h } from "./dom";
import { clock, plural, when } from "./format";
import { ReportView } from "./report";

type Screen = "welcome" | "setup" | "sync" | "report";

const root = document.getElementById("app")!;
let view: AppView;
let screen: Screen = "welcome";
let report: ReportView | null = null;
let checking = false;
let connecting = false;
let afterConnect: (() => void) | null = null;
let message: { text: string; kind: "info" | "warn" } | null = null;

const header = h("header", { class: "bar" });
const main = h("main", { id: "screen" });
const footer = h("footer", { class: "status", id: "status", "aria-live": "polite" });
root.append(header, main, footer);

window.addEventListener("error", (e) => void api.pageError(String(e.message)));
window.addEventListener("unhandledrejection", (e) => void api.pageError(String(e.reason)));

// ---------------------------------------------------------------- header

function renderHeader(): void {
  clear(header);
  const name = view.business_name && view.connected ? h("span", { class: "business" }, view.business_name) : null;
  const help = helpMenu();
  const settings =
    view.connected && view.settings.setup_done
      ? h("button", { class: "ghost", id: "open-settings", onclick: () => openSettings() }, "Settings")
      : null;
  header.append(
    h("div", { class: "bar-inner" }, h("span", { class: "brand" }, "Idle Stock Finder"), name, h("span", { class: "grow" }), settings, help),
  );
}

function helpMenu(): HTMLElement {
  const menu = h(
    "div",
    { class: "menu right", hidden: true, id: "help-menu" },
    h("button", { id: "help-self-test", onclick: () => pick(openSelfTest) }, "Check this computer"),
    h("button", { id: "help-diagnostics", onclick: () => pick(openDiagnostics) }, "Save diagnostics…"),
    h("button", { id: "help-about", onclick: () => pick(openAbout) }, "About Idle Stock Finder"),
  );
  const pick = (open: () => unknown) => {
    menu.hidden = true;
    void open();
  };
  document.addEventListener("click", (e) => {
    if (!(e.target as Element).closest(".help-anchor")) menu.hidden = true;
  });
  return h(
    "div",
    { class: "menu-anchor help-anchor" },
    h("button", { class: "ghost", id: "help", "aria-haspopup": "true", onclick: () => (menu.hidden = !menu.hidden) }, "Help"),
    menu,
  );
}

// ---------------------------------------------------------------- screens

function show(next: Screen): void {
  screen = next;
  clear(main);
  main.className = `screen-${next}`;
  if (next === "welcome") main.append(welcome());
  if (next === "setup") main.append(setup());
  if (next === "sync") main.append(syncing());
  if (next === "report") {
    if (!report) report = new ReportView(view);
    report.setView(view);
    main.append(report.el);
    void report.load();
  }
  renderHeader();
  renderStatus();
}

function route(): void {
  if (!view.connected) return show("welcome");
  if (!view.settings.setup_done) return show("setup");
  if (!view.status.synced) {
    show("sync");
    startCheck();
    return;
  }
  show("report");
}

function welcome(): HTMLElement {
  const lapsed = view.status.disconnected;
  const connect = h(
    "button",
    {
      class: "primary big",
      id: "connect",
      disabled: connecting || !view.configured,
      onclick: () => void doConnect(true),
    },
    "Connect Square",
  );
  return h(
    "section",
    { class: "welcome" },
    h("h1", {}, "Idle Stock Finder"),
    h(
      "p",
      { class: "lead" },
      "Finds the items in your shop that haven't sold lately, and labels each one in Square with the day it last sold.",
    ),
    lapsed ? h("p", { class: "warn" }, "Square ended the connection. Connect again to keep labels current.") : null,
    connect,
    h(
      "p",
      { class: "fine" },
      "Reads your items and sales. Writes one ‘Last sold’ label on your items. Nothing leaves this computer except requests to Square.",
    ),
    connecting
      ? h(
          "p",
          { class: "waiting", id: "connect-waiting" },
          "Finish in your browser: log in to Square and approve. ",
          h("button", { class: "link", onclick: () => void api.cancelConnect() }, "Cancel"),
        )
      : null,
    message ? h("p", { class: message.kind, id: "message" }, message.text) : null,
    view.configured ? null : h("p", { class: "warn" }, "This build isn't linked to a Square application yet."),
  );
}

async function doConnect(labelsOn: boolean, then?: () => void): Promise<void> {
  message = null;
  connecting = true;
  afterConnect = then ?? null;
  if (screen === "welcome") show("welcome");
  try {
    await api.connect(labelsOn);
  } catch (e) {
    connecting = false;
    message = { text: String(e), kind: "warn" };
    if (screen === "welcome") show("welcome");
  }
}

function timeOptions(selected: [number, number]): HTMLOptionElement[] {
  const out: HTMLOptionElement[] = [];
  for (let m = 0; m < 24 * 60; m += 15) {
    const hh = Math.floor(m / 60);
    const mm = m % 60;
    out.push(h("option", { value: `${hh}:${mm}`, selected: hh === selected[0] && mm === selected[1] }, clock(hh, mm)));
  }
  return out;
}

/** The form shared by setup and Settings. */
function settingsForm(initial: Settings): { el: HTMLElement; read: () => Settings } {
  const days = h("input", { type: "number", min: "1", max: "3650", value: String(initial.window_days), id: "set-window" });
  const presets = [30, 60, 90].map((d) =>
    h("button", { type: "button", class: "chip", onclick: () => (days.value = String(d)) }, `${d}`),
  );
  const active = view.locations.filter((l) => l.active);
  const inactive = view.locations.filter((l) => !l.active);
  const chosen = (id: string, isActive: boolean) => (initial.locations ? initial.locations.includes(id) : isActive);
  const boxes = view.locations.map((l) =>
    h("input", { type: "checkbox", value: l.id, checked: chosen(l.id, l.active), "data-active": String(l.active) }),
  );
  const locationList = h(
    "div",
    { class: "locations", id: "set-locations" },
    ...view.locations.map((l, i) =>
      h("label", { class: `check ${l.active ? "" : "inactive"}` }, boxes[i], ` ${l.name}`, l.active ? null : h("small", {}, " (inactive)")),
    ),
  );
  const labels = h("input", { type: "checkbox", id: "set-labels", checked: initial.labels_on, role: "switch" });
  const time = h("select", { id: "set-time" }, ...timeOptions(initial.check_time));
  const background = h("input", { type: "checkbox", id: "set-background", checked: initial.background_on, role: "switch" });
  const el = h(
    "div",
    { class: "form" },
    h(
      "div",
      { class: "row" },
      h("label", { for: "set-window" }, "Slow means no sale in"),
      h("div", {}, days, " days ", ...presets),
    ),
    view.locations.length > 1
      ? h("div", { class: "row" }, h("span", { class: "label" }, "Locations that count"), locationList)
      : null,
    h(
      "div",
      { class: "row" },
      h("label", { for: "set-labels" }, "Labels in Square"),
      h(
        "div",
        {},
        h("label", { class: "switch" }, labels, h("span", {}, "")),
        h("p", { class: "hint" }, "Writes one label on each item in Square, like “Last sold Jul 14, 2026”. Nothing else in Square changes."),
      ),
    ),
    h("div", { class: "row" }, h("label", { for: "set-time" }, "Daily check"), h("div", {}, time)),
    h(
      "div",
      { class: "row" },
      h("label", { for: "set-background" }, "Check in the background"),
      h(
        "div",
        {},
        h("label", { class: "switch" }, background, h("span", {}, "")),
        h("p", { class: "hint" }, "Checks once a day at that time and each time you sign in. Nothing stays running."),
      ),
    ),
    inactive.length && !active.length ? h("p", { class: "warn" }, "Every location is inactive in Square.") : null,
  );
  const read = (): Settings => {
    const picked = boxes.filter((b) => b.checked).map((b) => b.value);
    const activeIds = active.map((l) => l.id);
    const allActive = picked.length === activeIds.length && activeIds.every((id) => picked.includes(id));
    const [hh, mm] = time.value.split(":").map((x) => parseInt(x, 10));
    const d = parseInt(days.value, 10);
    return {
      window_days: Number.isFinite(d) && d > 0 ? d : initial.window_days,
      locations: view.locations.length <= 1 || allActive ? null : picked,
      labels_on: labels.checked,
      check_time: [hh, mm],
      background_on: background.checked,
      setup_done: initial.setup_done,
    };
  };
  return { el, read };
}

function setup(): HTMLElement {
  const form = settingsForm(view.settings);
  const start = h(
    "button",
    {
      class: "primary big",
      id: "start",
      onclick: async () => {
        const settings = form.read();
        if (settings.locations && settings.locations.length === 0) {
          message = { text: "Pick at least one location.", kind: "warn" };
          return show("setup");
        }
        const result = await api.saveSettings(settings, true);
        view = result.view;
        if (result.needs_reconnect) {
          return void doConnect(true, () => route());
        }
        message = result.registration_error
          ? { text: `Background checks couldn't be set up: ${result.registration_error}`, kind: "warn" }
          : null;
        route();
      },
    },
    "Start",
  );
  return h(
    "section",
    { class: "setup" },
    h("p", { class: "connected", id: "connected-to" }, `Connected to ${view.business_name ?? "Square"}.`),
    h("h2", {}, "Set up"),
    form.el,
    message ? h("p", { class: message.kind }, message.text) : null,
    start,
  );
}

const progress = { text: "Starting…", fraction: 0 };

function syncing(): HTMLElement {
  const bar = h("progress", { max: "1", value: String(progress.fraction), id: "sync-progress" });
  return h(
    "section",
    { class: "sync" },
    h("h2", {}, "Reading your Square account"),
    bar,
    h("p", { id: "sync-text" }, progress.text),
    message ? h("p", { class: message.kind }, message.text, " ", h("button", { class: "secondary", onclick: () => startCheck() }, "Try again")) : null,
  );
}

function onProgress(p: ProgressEvent): void {
  if (p.kind === "catalog") {
    progress.text = `Reading items: ${p.objects.toLocaleString()}`;
    progress.fraction = 0.05;
  } else if (p.kind === "sales") {
    progress.text = `Reading sales: ${p.months_done} of ${p.months_total} months`;
    progress.fraction = 0.1 + 0.8 * (p.months_done / Math.max(1, p.months_total));
  } else if (p.kind === "inventory") {
    progress.text = "Reading stock counts";
    progress.fraction = 0.92;
  } else {
    progress.text = "Writing labels in Square";
    progress.fraction = 0.96;
  }
  if (screen === "sync") {
    const bar = document.getElementById("sync-progress") as HTMLProgressElement | null;
    const text = document.getElementById("sync-text");
    if (bar) bar.value = progress.fraction;
    if (text) text.textContent = progress.text;
  }
  renderStatus();
}

function startCheck(): void {
  if (checking) return;
  checking = true;
  message = null;
  progress.text = "Connecting to Square";
  progress.fraction = 0.02;
  renderStatus();
  void api.checkNow();
}

async function onCheckDone(outcome: Outcome): Promise<void> {
  checking = false;
  view = await api.view();
  if (outcome.outcome === "failed") {
    message = { text: `The check didn't finish: ${outcome.detail}. Labels in Square stay as they are.`, kind: "warn" };
  } else if (outcome.outcome === "disconnected" || outcome.outcome === "not_connected") {
    message = null;
    return route();
  } else if (outcome.outcome === "done" && outcome.detail.label_problem) {
    message = { text: capitalise(outcome.detail.label_problem) + ".", kind: "warn" };
  } else {
    message = null;
  }
  if (screen === "sync" && view.status.synced) return show("report");
  if (screen === "sync") return show("sync");
  if (screen === "report" && report) {
    report.setView(view);
    void report.load();
  }
  renderStatus();
}

function capitalise(s: string): string {
  return s.charAt(0).toUpperCase() + s.slice(1);
}

// ---------------------------------------------------------------- status

function renderStatus(): void {
  clear(footer);
  if (!view?.connected || !view.settings.setup_done) {
    footer.hidden = true;
    return;
  }
  footer.hidden = false;
  const s = view.status;
  const parts: string[] = [`Connected to ${view.business_name ?? "Square"}`];
  if (checking) {
    parts.push(progress.text.toLowerCase());
  } else {
    const last = s.last_check;
    parts.push(last ? `last check ${when(last.ended_at ?? last.started_at)}${last.outcome === "failed" ? " (didn't finish)" : ""}` : "not checked yet");
    if (view.next_check) parts.push(`next ${when(view.next_check)}`);
    else parts.push("background checks off");
  }
  parts.push(view.settings.labels_on ? `labels on ${plural(s.labels_in_square, "item")}` : "labels off");
  footer.append(h("span", { id: "status-line" }, parts.join(" · ")));
  if (s.halted) {
    footer.append(
      h("button", { class: "link warn", id: "status-halted", onclick: () => openSettings() }, "Label writes stopped: review"),
    );
  }
  if (message && screen === "report") footer.append(h("span", { class: message.kind }, message.text));
  footer.append(
    h("span", { class: "grow" }),
    h("button", { class: "secondary", id: "check-now", disabled: checking, onclick: () => startCheck() }, checking ? "Checking…" : "Check now"),
  );
}

// ---------------------------------------------------------------- dialogs

function dialog(title: string, ...content: (Node | null)[]): HTMLDialogElement {
  const d = h(
    "dialog",
    { class: "dialog", "aria-label": title },
    h("div", { class: "dialog-head" }, h("h2", {}, title), h("button", { class: "ghost", "aria-label": "Close", onclick: () => d.close() }, "✕")),
    ...content.filter((c): c is Node => c !== null),
  );
  d.addEventListener("close", () => d.remove());
  document.body.append(d);
  d.showModal();
  return d;
}

function openSettings(): void {
  const form = settingsForm(view.settings);
  const note = h("p", { class: "hint", id: "settings-note" });
  const halted = view.status.halted
    ? h(
        "div",
        { class: "halt", id: "halt" },
        h("h3", {}, "Label writes are stopped"),
        h(
          "p",
          {},
          `On ${when(view.status.halted.at)}, Square stored more than the label on “${view.status.halted.name}”. ` +
            `What changed: ${view.status.halted.paths.join(", ")}. No other labels have been written since.`,
        ),
        h(
          "button",
          {
            class: "secondary",
            onclick: async () => {
              const details = await api.haltDetails();
              if (!details) return;
              dialog(
                "What changed",
                h("p", {}, "Before the write, what was sent, and what Square stored:"),
                h("pre", { class: "evidence" }, JSON.stringify({ before: details.fetched, sent: details.sent, stored: details.stored }, null, 2)),
              );
            },
          },
          "Show before and after",
        ),
        " ",
        h(
          "button",
          {
            class: "primary",
            id: "resume-labels",
            onclick: async () => {
              view = await api.resumeLabels();
              d.close();
              renderStatus();
            },
          },
          "Resume labels",
        ),
      )
    : null;
  const save = h(
    "button",
    {
      class: "primary",
      id: "settings-save",
      onclick: async () => {
        const next = form.read();
        const changedLabels = next.window_days !== view.settings.window_days || JSON.stringify(next.locations) !== JSON.stringify(view.settings.locations) || next.labels_on !== view.settings.labels_on;
        const result = await api.saveSettings(next, false);
        view = result.view;
        if (result.needs_reconnect) {
          d.close();
          return void doConnect(true, () => startCheck());
        }
        if (result.registration_error) {
          note.textContent = `Background checks couldn't be changed: ${result.registration_error}`;
          return;
        }
        d.close();
        report = null;
        show("report");
        if (changedLabels) startCheck();
      },
    },
    "Save",
  );
  const remove = h(
    "button",
    {
      class: "danger",
      id: "remove-labels",
      onclick: () => {
        if (!window.confirm("Remove the Last sold label from every item in Square? Nothing else in Square changes. Labels stay off until you turn them on again.")) return;
        note.textContent = "Removing labels…";
        void api.removeLabels();
      },
    },
    "Remove all labels from Square",
  );
  const disconnect = h(
    "button",
    {
      class: "danger",
      id: "disconnect",
      onclick: async () => {
        if (!window.confirm("Disconnect Square? The connection is deleted from this computer and checks stop.")) return;
        view = await api.disconnect();
        d.close();
        report = null;
        message = {
          text: "Disconnected. To remove Idle Stock Finder from Square too, open Square Dashboard, then Settings, then App integrations.",
          kind: "info",
        };
        route();
      },
    },
    "Disconnect Square",
  );
  const d = dialog(
    "Settings",
    halted,
    form.el,
    h("div", { class: "actions" }, save),
    note,
    h("h3", {}, "Square"),
    h("div", { class: "actions" }, remove, disconnect),
    h(
      "p",
      { class: "hint" },
      "You can also cut Idle Stock Finder off from Square at any time: Square Dashboard, Settings, App integrations, Disconnect App.",
    ),
  );
}

function openSelfTest(): void {
  const list = h("ul", { class: "steps", id: "self-test-steps" }, h("li", {}, "Running…"));
  const d = dialog("Check this computer", h("p", {}, "Runs a full check against built-in test data on this computer. Nothing is sent anywhere."), list);
  void api.selfTest();
  void on<SelfTestReport>("self-test-done", (r) => {
    if (!d.open) return;
    clear(list);
    for (const s of r.steps) {
      list.append(h("li", { class: s.ok ? "ok" : "warn" }, `${s.ok ? "✓" : "✗"} ${s.name}`, s.detail ? h("small", {}, ` ${s.detail}`) : null));
    }
    list.append(h("li", { class: r.passed ? "ok" : "warn", id: "self-test-result" }, r.passed ? `All good (${r.millis} ms).` : "Something isn't working. Save diagnostics and send them to us."));
  });
}

async function openDiagnostics(): Promise<void> {
  const text = await api.diagnostics();
  const area = h("textarea", { readonly: true, class: "diagnostics", id: "diagnostics-text" });
  area.value = text;
  const note = h("p", { class: "hint" });
  dialog(
    "Save diagnostics",
    h("p", {}, "This is everything the file will contain. Item names, amounts and ids are replaced with placeholders."),
    area,
    h(
      "div",
      { class: "actions" },
      h(
        "button",
        {
          class: "primary",
          onclick: async () => {
            const path = await api.saveDiagnostics(text);
            if (path) note.textContent = `Saved to ${path}`;
          },
        },
        "Save…",
      ),
    ),
    note,
  );
}

function openAbout(): void {
  dialog(
    "About Idle Stock Finder",
    h("p", {}, `Version ${view.version}. A free tool from Tight Line.`),
    h("p", {}, "It talks only to Square. No tracking, no update checks, no other servers. Your sales stay on this computer."),
    h("p", { class: "hint", id: "about-background" }, `Background check: ${view.registration.detail}`),
  );
}

// ---------------------------------------------------------------- events

async function boot(): Promise<void> {
  view = await api.view();
  await on<{ business_name: string }>("connected", async () => {
    connecting = false;
    message = null;
    view = await api.view();
    const next = afterConnect;
    afterConnect = null;
    if (next) next();
    else route();
  });
  await on<string>("connect-failed", (text) => {
    connecting = false;
    message = { text, kind: "warn" };
    if (screen === "welcome") show("welcome");
    else renderStatus();
  });
  await on<ProgressEvent>("progress", onProgress);
  await on<Outcome>("check-done", (o) => void onCheckDone(o));
  await on<{ ok: boolean; message: string }>("remove-done", async (r) => {
    view = await api.view();
    message = { text: r.message, kind: r.ok ? "info" : "warn" };
    const note = document.getElementById("settings-note");
    if (note) note.textContent = r.message;
    if (screen === "report" && report) void report.load();
    renderStatus();
  });
  route();
}

void boot();
