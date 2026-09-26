if (!window.__TAURI__) {
  const boot = document.createElement("div");
  boot.textContent =
    "The Tauri bridge is missing. Set app.withGlobalTauri: true in tauri.conf.json.";
  // Set from script: a style attribute here would be dropped by the CSP, and
  // this message has to be legible precisely when things are broken.
  boot.style.cssText = "padding:60px;font:14px system-ui;color:#B33A35";
  document.body.innerHTML = "";
  document.body.append(boot);
  throw new Error("window.__TAURI__ is undefined");
}

const { invoke } = window.__TAURI__.core;
const { listen }  = window.__TAURI__.event;
const { openUrl } = window.__TAURI__.opener;
const dialog      = window.__TAURI__.dialog;
const clipboard   = window.__TAURI__.clipboardManager;
const { getCurrentWindow } = window.__TAURI__.window;

// A silent exception in here looks exactly like "the buttons do nothing", so
// surface anything that escapes.
window.addEventListener("error", (e) =>
  toast("Error: " + (e.error?.message || e.message)));
window.addEventListener("unhandledrejection", (e) =>
  toast("Error: " + (e.reason?.message || e.reason)));

const $ = (s) => document.querySelector(s);

/// Filled in from the app itself at start-up, so the About page cannot drift
/// from tauri.conf.json.
let APP_VERSION = "";
window.__TAURI__.app?.getVersion?.()
  .then((v) => { APP_VERSION = v; })
  .catch(() => {});
const THEMES = ["system", "system2", "dark", "gray", "green", "orange", "pink", "purple"];

/// Set while the Shortcuts page is waiting for a key, so the key is
/// recorded instead of acted on.
let keyCapture = null;

const state = {
  /// null until the user picks a feed, folder or category: the list starts
  /// empty rather than guessing what to show.
  scope: null,
  scopeName: "",
  items: [],
  /// Which article the reading pane is showing.
  selected: null,
  /// The ArticleView for `selected`, kept so the toolbar still knows the
  /// starred state after the row drops out of the list.
  current: null,
  /// Which articles toolbar actions apply to. Usually mirrors `selected`,
  /// but ctrl- and shift-click let it hold many.
  sel: new Set(),
  /// Where a shift-click range starts from.
  anchor: null,
  tree: [],
  labels: [],
  query: "",
  /// More rows exist past the ones loaded; scrolling near the end fetches
  /// the next page.
  more: false,
  loadingMore: false,
  /// Site icons by feed id, as data: URLs. Feeds without one show a letter.
  icons: {},
  /// `date`, `title`, `author` or `feed`; a leading `-` means descending.
  sort: (() => { try { return localStorage.getItem("sort") || "-date"; } catch { return "-date"; } })(),
  /// scope|query|sort of the rows in `items`, so a reload of the same list
  /// keeps as many rows as were loaded instead of snapping back to one page.
  listKey: "",
};

/// The focused article's list row, or null. It can legitimately be missing:
/// open an article in Unread, it is marked read, and the next list reload
/// drops it. Everything that reaches for it has to cope with that — not
/// coping is what threw "cannot read properties of undefined".
function focusedItem() {
  return state.items.find((x) => x.id === state.selected) || null;
}

/// What a toolbar action should act on: the selection if there is one, else
/// whatever the reading pane is showing.
function targetIds() {
  if (state.sel.size) return [...state.sel];
  return state.selected ? [state.selected] : [];
}

/// Rows for ids that are still in the list. Ids that are not (already
/// deleted, filtered out) are simply absent rather than undefined.
function itemsFor(ids) {
  const want = new Set(ids);
  return state.items.filter((i) => want.has(i.id));
}

function setSelection(ids, { anchor } = {}) {
  state.sel = new Set(ids);
  if (anchor !== undefined) state.anchor = anchor;
  renderSelection();
}

/// The selection as the rows on screen show it. Only rows whose state
/// changed are touched: going over every row on each keypress got slow with
/// thousands of rows loaded.
let shownSel = new Set();
function renderSelection() {
  if ($("#list")) {
    for (const id of shownSel) if (!state.sel.has(id)) { const n = rowNode(id); if (n) n.dataset.sel = "false"; }
    for (const id of state.sel) if (!shownSel.has(id)) { const n = rowNode(id); if (n) n.dataset.sel = "true"; }
    shownSel = new Set(state.sel);
  }
  const badge = $("#selcount");
  if (badge) {
    badge.textContent = state.sel.size > 1 ? `${state.sel.size} selected` : "";
    badge.classList.toggle("show", state.sel.size > 1);
  }
}

/// WebView2 does not implement window.prompt at all, and confirm is
/// unreliable across Tauri's platforms. Both are replaced with an in-app
/// modal that behaves the same everywhere.
/// True while any dialog is up. A second one is refused rather than stacked:
/// otherwise a button clicked again while its dialog is still open spawns
/// another, and another, each darkening the backdrop further.
function modalOpen() {
  return !!document.querySelector(".modal-back");
}

function ask({ title, placeholder = "", value = "", confirmLabel = "OK", danger = false }) {
  if (modalOpen()) return Promise.resolve(null);
  return new Promise((resolve) => {
    const back = document.createElement("div");
    back.className = "modal-back";
    // Escaped: titles carry feed and label names, which can contain quotes.
    back.innerHTML = `
      <div class="modal" role="dialog" aria-modal="true" aria-label="${esc(title)}">
        <div class="modal-title">${esc(title)}</div>
        ${placeholder !== null
          ? `<input class="modal-input" placeholder="${esc(placeholder)}" value="${esc(value)}" spellcheck="false" autocomplete="off">`
          : ""}
        <div class="modal-row">
          <button class="pill" data-cancel>Cancel</button>
          <button class="primary${danger ? " danger" : ""}" data-ok>${confirmLabel}</button>
        </div>
      </div>`;
    document.body.append(back);

    const input = back.querySelector(".modal-input");
    const done = (v) => { back.remove(); document.removeEventListener("keydown", key, true); resolve(v); };
    const key = (e) => {
      if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); done(null); }
      if (e.key === "Enter") {
        // Enter on Cancel is a press of Cancel, which the button does itself.
        // Taking every Enter as OK meant tabbing to Cancel and pressing Enter
        // removed the feed anyway.
        const f = document.activeElement;
        if (f && back.contains(f) && f !== input && !f.matches("[data-ok]")) return;
        e.preventDefault(); e.stopPropagation(); done(input ? input.value.trim() : true);
      }
    };

    back.querySelector("[data-cancel]").onclick = () => done(null);
    back.querySelector("[data-ok]").onclick = () => done(input ? input.value.trim() : true);
    back.onclick = (e) => { if (e.target === back) done(null); };
    document.addEventListener("keydown", key, true);
    if (input) { input.focus(); input.select(); } else back.querySelector("[data-ok]").focus();
  });
}

/// A list of choices in a modal; resolves to the chosen value, or null.
function choose({ title, options }) {
  if (modalOpen()) return Promise.resolve(null);
  return new Promise((resolve) => {
    const back = document.createElement("div");
    back.className = "modal-back";
    back.innerHTML = `
      <div class="modal" role="dialog" aria-modal="true" aria-label="${esc(title)}">
        <div class="modal-title">${esc(title)}</div>
        <div class="choices">
          ${options.map((o, i) => `
            <button class="choice" data-choice="${i}"${o.disabled ? " disabled" : ""}>
              <span class="c1">${esc(o.label)}${o.note ? ` <span class="faint">· ${esc(o.note)}</span>` : ""}</span>
              ${o.sub ? `<span class="c2">${esc(o.sub)}</span>` : ""}
            </button>`).join("")}
        </div>
        <div class="modal-row"><button class="pill" data-cancel>Cancel</button></div>
      </div>`;
    document.body.append(back);
    const done = (v) => { back.remove(); document.removeEventListener("keydown", key, true); resolve(v); };
    const key = (e) => { if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); done(null); } };
    back.querySelectorAll("[data-choice]").forEach((b) => {
      b.onclick = () => done(options[+b.dataset.choice].value);
    });
    back.querySelector("[data-cancel]").onclick = () => done(null);
    back.onclick = (e) => { if (e.target === back) done(null); };
    document.addEventListener("keydown", key, true);
    back.querySelector("[data-choice]:not([disabled])")?.focus();
  });
}

function toast(msg) {
  const t = $("#toast");
  t.textContent = msg;
  t.classList.add("show");
  clearTimeout(t._h);
  t._h = setTimeout(() => t.classList.remove("show"), 2200);
}
/// Apply the colours carried on `data-bg` / `data-fg`.
///
/// Tauri rewrites the configured CSP and puts a nonce on `style-src`. Under
/// CSP Level 3 a nonce makes `'unsafe-inline'` inert for *style attributes as
/// well as style elements*, so every colour this app wrote into a style
/// attribute inside innerHTML was silently dropped — which is why feed
/// avatars, label swatches and label chips all rendered with no colour.
///
/// CSP does not govern the CSSOM, so setting the property from script works
/// everywhere. Anything whose colour is computed at runtime goes through here;
/// anything with a fixed colour belongs in the stylesheet.
function paint(root) {
  (root || document).querySelectorAll("[data-bg]").forEach((el) => {
    el.style.background = el.dataset.bg;
    if (el.dataset.fg) el.style.color = el.dataset.fg;
  });
}

function esc(s) {
  return String(s ?? "").replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}
function tint(str) {
  let h = 0;
  for (const ch of String(str)) h = (h * 31 + ch.charCodeAt(0)) % 360;
  // Commas, not spaces. The space-separated form is CSS Color 4, and the
  // WebKitGTK builds on older distributions drop the declaration outright —
  // which is why every feed's avatar was a bare letter on no background.
  return `hsl(${h}, 32%, 34%)`;
}
function when(iso) {
  if (!iso) return "";
  const d = new Date(iso);
  if (isNaN(d)) return "";
  const days = daysAgo(d);
  if (days <= 0) return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  if (days < 7) return d.toLocaleDateString([], { weekday: "short" }) + " " +
                       d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  return d.toLocaleDateString([], { day: "numeric", month: "short" });
}
/// Calendar days between `d` and today: 0 today, 1 yesterday. Counting
/// 24-hour periods back from midnight made yesterday 0, so yesterday's
/// articles were grouped under Today and shown with no day.
function daysAgo(d) {
  const a = new Date(d); a.setHours(0, 0, 0, 0);
  const t = new Date(); t.setHours(0, 0, 0, 0);
  return Math.round((t - a) / 86400000);
}
function dayKey(iso) {
  const d = new Date(iso); if (isNaN(d)) return "Earlier";
  const days = daysAgo(d);
  if (days <= 0) return "Today";
  if (days === 1) return "Yesterday";
  if (days < 7) return "This week";
  return "Earlier";
}

const STAR_OUTLINE = '<svg width="14" height="14" viewBox="0 0 16 16" fill="none"><path d="M8 2.3l1.75 3.66 3.95.55-2.87 2.78.7 3.99L8 11.4l-3.53 1.88.7-3.99L2.3 6.51l3.95-.55z" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round"/></svg>';
/// The toolbar star has to change shape as well as colour: a stroke-only
/// glyph in a warm colour still reads as "not starred".
function setStarButton(el, on) {
  if (!el) return;
  el.classList.toggle("on", !!on);
  // Only the icon changes: replacing the whole button dropped its text label,
  // and the tooltip named S even when the shortcut had been changed.
  const t = document.createElement("template");
  t.innerHTML = on ? STAR_SOLID : STAR_OUTLINE;
  const icon = t.content.firstElementChild;
  icon.setAttribute("aria-hidden", "true");
  const old = el.querySelector("svg");
  if (old) {
    icon.setAttribute("width", old.getAttribute("width"));
    icon.setAttribute("height", old.getAttribute("height"));
    old.replaceWith(icon);
  } else el.prepend(icon);
  el.setAttribute("aria-pressed", String(!!on));
  // What refreshKeyTips reads, so a later shortcut change keeps "Unstar".
  el.dataset.keyaction = "star";
  el.dataset.tip = on ? "Unstar" : "Star";
  const k = keyHint("star");
  el.title = k ? `${el.dataset.tip} (${k})` : el.dataset.tip;
}

/// A filled blue disc for unread, a hollow ring for read. Rendering both as
/// the same element keeps the click target in one place.
const DOT = '<i></i>';

const STAR_SOLID   = '<svg width="14" height="14" viewBox="0 0 16 16"><path d="M8 2.3l1.75 3.66 3.95.55-2.87 2.78.7 3.99L8 11.4l-3.53 1.88.7-3.99L2.3 6.51l3.95-.55z" fill="currentColor"/></svg>';

// ------------------------------------------------------------------ feed tree
async function loadTree() {
  const [tree, c, labelRows] = await Promise.all([
    invoke("feed_tree"),
    invoke("counts"),
    invoke("labels").catch(() => []),
  ]);
  state.tree = tree;
  state.labels = labelRows;

  // --- feeds ---------------------------------------------------------------
  const el = $("#tree");
  el.innerHTML = "";
  if (tree.length) {
    const walk = (nodes, depth) => {
      for (const n of nodes) {
        el.append(row({
          scope: (n.is_folder ? "folder:" : "feed:") + n.id,
          label: n.title, count: n.unread, depth, node: n,
        }));
        if (n.children.length && n.expanded !== false) walk(n.children, depth + 1);
      }
    };
    walk(tree, 0);
  } else {
    el.insertAdjacentHTML("beforeend",
      '<div class="empty nofeeds">No feeds yet.</div>');
  }

  // --- categories, below the feeds ----------------------------------------
  const cats = $("#catlist");
  cats.innerHTML = "";
  // The tone is a class rather than a colour, because a colour would have to
  // ride in on a style attribute and those do not survive the CSP.
  const smart = [
    ["unread", "Unread", c.unread, ICON_DOT, "unread"],
    ["all", "All articles", c.total, ICON_INBOX, "dim"],
    ["starred", "Starred", c.starred, ICON_STAR, "star"],
    ["deleted", "Deleted", null, ICON_TRASH, "dim"],
  ];
  for (const [scope, label, n, icon, tone] of smart)
    cats.append(row({ scope, label, count: n, depth: 0, icon, tone }));

  if (labelRows.length) {
    cats.insertAdjacentHTML("beforeend", '<div class="caption labcap">LABELS</div>');
    for (const l of labelRows) {
      const r = row({
        scope: "label:" + l.id, label: l.name, count: l.count, depth: 0,
        swatch: l.color_bg || "var(--ink-4)",
      });
      r.dataset.labelId = String(l.id);
      r.ondblclick = (e) => { e.preventDefault(); editLabelById(l.id); };
      cats.append(r);
    }
  }
}

const ICON_INBOX = '<path d="M2 9.5h3l1 1.8h4l1-1.8h3M2.6 9.2l1.8-5.1a1 1 0 0 1 .95-.7h5.3a1 1 0 0 1 .95.7l1.8 5.1v2.9a1 1 0 0 1-1 1H3.6a1 1 0 0 1-1-1z" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round"/>';
const ICON_DOT = '<circle cx="8" cy="8" r="3.4" fill="currentColor"/>';
const ICON_STAR = '<path d="M8 2.3l1.75 3.66 3.95.55-2.87 2.78.7 3.99L8 11.4l-3.53 1.88.7-3.99L2.3 6.51l3.95-.55z" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round"/>';
const ICON_TRASH = '<path d="M2.6 4.4h10.8M4 4.4l.7 8.1a1 1 0 0 0 1 .9h4.6a1 1 0 0 0 1-.9l.7-8.1M6 4.4V3a.9.9 0 0 1 .9-.9h2.2a.9.9 0 0 1 .9.9v1.4" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"/>';

/// A feed's site icon, or a coloured letter when it has none.
function feedIcon(feedId, title, cls) {
  const src = state.icons[feedId];
  if (src) return `<img class="favicon img ${cls}" src="${esc(src)}" alt="" draggable="false">`;
  return `<span class="favicon ${cls}" data-bg="${esc(tint(title))}">${esc((title || "?").trim()[0] || "?").toUpperCase()}</span>`;
}

// ------------------------------------------------------------ text size
// The reading pane's text size, per machine. Applied with CSS zoom on the
// article column, as a browser's zoom would, so images and the column
// width grow with the text.
const TEXT_STEPS = [0.8, 0.9, 1, 1.1, 1.2, 1.35, 1.5, 1.75, 2];
function textScale() {
  let v = 1;
  try { v = Number(localStorage.getItem("textScale")) || 1; } catch {}
  return TEXT_STEPS.includes(v) ? v : 1;
}
function setTextScale(v, { quiet = false } = {}) {
  document.documentElement.style.setProperty("--text-scale", String(v));
  try { localStorage.setItem("textScale", String(v)); } catch {}
  if (!quiet) toast(`Text size ${Math.round(v * 100)}%`);
  const out = document.querySelector("[data-textsize]");
  if (out) out.textContent = `${Math.round(v * 100)}%`;
}
function textBigger() {
  const i = TEXT_STEPS.indexOf(textScale());
  setTextScale(TEXT_STEPS[Math.min(TEXT_STEPS.length - 1, i + 1)]);
}
function textSmaller() {
  const i = TEXT_STEPS.indexOf(textScale());
  setTextScale(TEXT_STEPS[Math.max(0, i - 1)]);
}
function textReset() { setTextScale(1); }

/// Icons arrive as data: URLs, which can be tens of KB each. Written into
/// every row they made the list's HTML megabytes long, rebuilt on each
/// click; turned into short blob: URLs once, each row carries a reference.
async function loadIcons() {
  let raw = {};
  try { raw = await invoke("feed_icons") || {}; } catch {}
  const old = state.icons;
  const next = {};
  const kept = new Set();
  for (const [id, url] of Object.entries(raw)) {
    // An icon that has not changed keeps its address, so the rows showing it
    // are left alone when a batch of new icons arrives.
    if (iconData[id] === url && old[id]) { next[id] = old[id]; kept.add(old[id]); }
    else next[id] = blobUrl(url) || url;
  }
  iconData = raw;
  state.icons = next;
  for (const u of Object.values(old)) if (u.startsWith("blob:") && !kept.has(u)) URL.revokeObjectURL(u);
}
let iconData = {};
function blobUrl(dataUrl) {
  try {
    if (typeof URL.createObjectURL !== "function") return null;
    const m = dataUrl.match(/^data:([^;,]+);base64,(.*)$/);
    if (!m) return null;
    const bin = atob(m[2]);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    return URL.createObjectURL(new Blob([bytes], { type: m[1] }));
  } catch { return null; }
}

function row(opts) {
  const { scope, label, count, depth, node } = opts;
  // A div, not a button. Chromium refuses to start a drag from a form control,
  // so tree rows built as <button> were undraggable in WebView2 however the
  // draggable attribute was set. role and tabindex keep it a button to
  // assistive tech and to the keyboard.
  const b = document.createElement("div");
  b.className = "node";
  b.setAttribute("role", "button");
  b.tabIndex = 0;
  b.style.paddingLeft = 8 + depth * 15 + "px";
  b.setAttribute("aria-current", state.scope === scope ? "true" : "false");
  b.dataset.scope = scope;
  b.dataset.label = label;
  if (node) b.dataset.id = node.id;

  let icon;
  if (node) {
    icon = node.is_folder
      ? `<span class="twisty toggle" data-toggle="${node.id}" role="button" aria-label="${node.expanded === false ? "Expand" : "Collapse"}" aria-expanded="${node.expanded !== false}"><svg width="12" height="12" viewBox="0 0 16 16" fill="none"><path d="M4 6.5L8 10.5l4-4" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg></span>`
      : feedIcon(node.id, label, "");
  } else if (opts.swatch) {
    icon = `<span class="twisty dotwrap"><span class="swatch-dot" data-bg="${esc(opts.swatch)}"></span></span>`;
  } else if (opts.icon) {
    icon = `<svg class="caticon" width="15" height="15" viewBox="0 0 16 16" fill="none" aria-hidden="true" data-tone="${esc(opts.tone || "")}">${opts.icon}</svg>`;
  } else {
    icon = '<span class="twisty"></span>';
  }

  const tail = node?.broken
    ? `<span class="warnicon" title="Last update failed"><svg width="12" height="12" viewBox="0 0 16 16" fill="none"><circle cx="8" cy="8" r="6" stroke="currentColor" stroke-width="1.3"/><path d="M8 5.2v3.6M8 11.1v.1" stroke="currentColor" stroke-width="1.7" stroke-linecap="round"/></svg></span>`
    : `<span class="count">${count ? count : ""}</span>`;

  b.innerHTML = `${icon}<span class="label">${esc(label)}</span>${tail}`;
  paint(b);
  b.onclick = (e) => {
    // The chevron folds the folder; the rest of the row opens it.
    if (e.target.closest("[data-toggle]")) {
      e.stopPropagation();
      return toggleFolder(node);
    }
    selectScope(scope, label);
  };
  b.onkeydown = (e) => {
    if (e.key === "Enter" || e.key === " ") { e.preventDefault(); b.click(); }
    // Left and right fold and unfold, as in any tree.
    if (node?.is_folder && e.key === "ArrowLeft" && node.expanded !== false) {
      e.preventDefault(); toggleFolder(node, false);
    }
    if (node?.is_folder && e.key === "ArrowRight" && node.expanded === false) {
      e.preventDefault(); toggleFolder(node, true);
    }
  };
  // Only real feeds and folders move. The smart categories and labels below
  // them are views, not places, so they are not drop targets either.
  if (node) makeDraggable(b, node);
  return b;
}

/// Fold or unfold a folder. Persisted on the feed row, so it survives a
/// restart and a QuiteRSS import keeps what was collapsed there.
async function toggleFolder(node, to) {
  const next = to ?? node.expanded === false;
  node.expanded = next;
  try { await invoke("set_expanded", { id: node.id, expanded: next }); }
  catch (e) { toast(String(e)); }
  await loadTree();
  // Keep keyboard focus on the folder that was toggled.
  document.querySelector(`#tree .node[data-id="${node.id}"]`)?.focus();
}

// --------------------------------------------------------- drag and drop
//
// Three drops are possible on one row: above it, below it, or into it when it
// is a folder. The row is split into thirds vertically, which is the same
// gesture every file tree uses, and each gets its own indicator because a
// single highlight would leave the user guessing which one they are about to
// get.

let dragId = null;
let springTimer = null;
let springId = null;

function makeDraggable(el, node) {
  el.draggable = true;
  el.dataset.folder = String(node.is_folder);

  // Double-click opens the feed's properties, or renames a folder.
  el.ondblclick = (e) => {
    e.preventDefault();
    // Two quick clicks on the chevron are two toggles, not a rename.
    if (e.target.closest?.("[data-toggle]")) return;
    if (node.is_folder) {
      ask({ title: "Rename folder", placeholder: "Name", value: node.title })
        .then((name) => { if (name) invoke("rename_node", { id: node.id, name }).then(() => renamed(node.id, name)); });
    } else {
      openFeedSettings(node.id);
    }
  };

  el.ondragstart = (e) => {
    dragId = node.id;
    el.classList.add("dragging");
    e.dataTransfer.effectAllowed = "move";
    // Firefox refuses to start a drag without data on the transfer.
    e.dataTransfer.setData("text/plain", String(node.id));
  };
  el.ondragend = () => {
    clearTimeout(springTimer); springId = null;
    dragId = null;
    el.classList.remove("dragging");
    clearDropMarks();
  };

  el.ondragover = (e) => {
    if (dragId === null || dragId === node.id) return;
    // preventDefault is what marks this element as a drop target. Without it
    // on *every* dragover the browser refuses the drop.
    e.preventDefault();
    e.stopPropagation();
    if (e.dataTransfer) e.dataTransfer.dropEffect = "move";
    clearDropMarks();
    const zone = dropZone(e, el, node.is_folder);
    el.classList.add("drop-" + zone);
    // Over a different row now: a folder about to spring open no longer is.
    if (springId !== null && springId !== node.id) {
      clearTimeout(springTimer); springId = null;
    }
    // Hovering over a collapsed folder opens it after a moment, so something
    // can be dropped beside one of its feeds rather than only at the end.
    if (node.is_folder && node.expanded === false && zone === "into") {
      if (springId !== node.id) {
        clearTimeout(springTimer);
        springId = node.id;
        springTimer = setTimeout(() => toggleFolder(node, true), 700);
      }
    } else if (springId === node.id) {
      clearTimeout(springTimer); springId = null;
    }
  };
  el.ondragenter = (e) => {
    if (dragId === null) return;
    e.preventDefault();
  };
  // dragleave also fires when the pointer crosses into a child element, so
  // only clear when it has really left the row.
  el.ondragleave = (e) => {
    if (el.contains(e.relatedTarget)) return;
    el.classList.remove("drop-before", "drop-after", "drop-into");
    if (springId === node.id) { clearTimeout(springTimer); springId = null; }
  };

  el.ondrop = async (e) => {
    if (dragId === null || dragId === node.id) return;
    e.preventDefault();
    e.stopPropagation();
    const zone = dropZone(e, el, node.is_folder);
    const id = dragId;
    dragId = null;
    clearDropMarks();
    try {
      await invoke("move_node", { id, target: node.id, whereTo: zone });
      await loadTree();
    } catch (err) {
      // The backend refuses a folder dropped inside itself, among other
      // things. Say so rather than leaving the tree looking unchanged for no
      // apparent reason.
      toast(String(err));
    }
  };
}

/// Where in the row the pointer is. A feed has no inside, so its row splits in
/// half; a folder reserves its middle for "drop inside".
///
/// The middle band is wide on purpose. Rows are 27px, and 20px in compact, so
/// thirds gave "inside" a 7px target — small enough that dropping into a
/// folder mostly missed. Dropping *beside* a folder is the rarer intent, so
/// the edges get 6px each and the folder keeps the rest.
function dropZone(e, el, isFolder) {
  const r = el.getBoundingClientRect();
  const y = e.clientY - r.top;
  if (!isFolder) return y < r.height / 2 ? "before" : "after";
  const edge = Math.min(6, r.height / 3);
  if (y < edge) return "before";
  if (y > r.height - edge) return "after";
  return "into";
}

function clearDropMarks() {
  document.querySelectorAll(".drop-before, .drop-after, .drop-into")
    .forEach((n) => n.classList.remove("drop-before", "drop-after", "drop-into"));
  $("#tree")?.classList.remove("drop-root");
}

/// Empty space below the tree is the root: dropping there takes a feed out of
/// whatever folder it was in, which is otherwise surprisingly hard to do.
function wireTreeRootDrop() {
  const tree = $("#tree");
  if (!tree) return;
  tree.ondragover = (e) => {
    if (dragId === null || e.target.closest(".node")) return;
    e.preventDefault();
    clearDropMarks();
    tree.classList.add("drop-root");
  };
  tree.ondragleave = () => tree.classList.remove("drop-root");
  tree.ondrop = async (e) => {
    if (dragId === null || e.target.closest(".node")) return;
    e.preventDefault();
    const id = dragId;
    dragId = null;
    clearDropMarks();
    try {
      await invoke("move_node", { id, target: null, whereTo: "into" });
      await loadTree();
    } catch (err) { toast(String(err)); }
  };
}

async function selectScope(scope, label) {
  // Another feed opens at its top. Kept scrolled, it opened mid-list and could
  // ask for its second page straight away.
  if (scope !== state.scope) $("#list").scrollTop = 0;
  state.scope = scope;
  state.scopeName = label;
  // A selection from the previous view means nothing here.
  state.sel = new Set();
  state.anchor = null;
  document.querySelectorAll(".node").forEach((n) =>
    n.setAttribute("aria-current", n.dataset.scope === scope ? "true" : "false"));
  $("#scopename").textContent = label;
  await loadList();
}

/// Back to "nothing selected": empty list, empty reading pane.
async function clearScope() {
  state.scope = null;
  state.scopeName = "";
  state.selected = null;
  state.current = null;
  document.querySelectorAll(".node").forEach((n) => n.setAttribute("aria-current", "false"));
  $("#scopename").textContent = "";
  $("#article").innerHTML = '<div class="empty">Select an article.</div>';
  await loadList();
}

// ------------------------------------------------------------------ list pane
let loadListTicket = 0;
const PAGE = 500;
/// The most rows a reload asks for, the backend's own limit.
const MAX_ROWS = 20000;

/// What the list shows: the chosen scope, or everything when there is none
/// but the search box has something in it.
function listScope() {
  return state.scope || (state.query.trim() ? "all" : null);
}

/// Which list the scope, search and sort describe; state.listKey is the one
/// on screen.
function listKeyNow() {
  return `${listScope()}|${state.query.trim()}|${state.sort}`;
}

function fetchList(scope, offset, limit) {
  return invoke("news_list", {
    scope,
    limit,
    offset,
    excerpts: document.documentElement.dataset.layout === "newspaper",
    query: state.query.trim() || null,
    sort: state.sort,
  });
}

async function loadList() {
  const scope = listScope();
  if (!scope) {
    state.items = [];
    state.sel = new Set();
    state.anchor = null;
    state.more = false;
    state.listKey = "";
    renderList();
    return;
  }
  // Two quick clicks in the tree can answer out of order. Only the reply to
  // the latest request is used, or the list showed the previous feed under
  // the new one's name, and bulk actions went to the wrong articles.
  const ticket = ++loadListTicket;
  const key = listKeyNow();
  // Reloading the same list (after an update, a delete, marking read) keeps
  // every row that was loaded, so the list does not jump back to one page.
  const limit = key === state.listKey ? Math.min(MAX_ROWS, Math.max(PAGE, state.items.length)) : PAGE;
  const items = await fetchList(scope, 0, limit);
  if (ticket !== loadListTicket || scope !== listScope()) return;
  const same = key === state.listKey && items.length === state.items.length
    && items.every((it, k) => it.id === state.items[k].id) && !!$("#list .item")
    && drawnLayout === document.documentElement.dataset.layout;
  const changed = same
    ? items.filter((it, k) => JSON.stringify(it) !== JSON.stringify(state.items[k])).map((i) => i.id)
    : null;
  state.items = items;
  state.more = items.length === limit;
  state.listKey = key;
  // A reload can drop rows: read articles leave the Unread scope, deleted ones
  // leave every scope. Stale ids in the selection would make bulk actions
  // operate on things the user can no longer see.
  const live = new Set(state.items.map((i) => i.id));
  state.sel = new Set([...state.sel].filter((id) => live.has(id)));
  if (state.anchor !== null && !live.has(state.anchor)) state.anchor = null;
  // A reload after a background update usually brings back the same
  // articles in the same order. Redrawing every row then froze the window
  // for a second with thousands loaded, every time an update ran.
  if (same) {
    redrawRows(changed);
    renderSelection();
    // Articles can arrive below the loaded ones (sorted oldest first, or an
    // old date), so whether there are more is asked again each time.
    const marker = $("#list .more");
    if (state.more && !marker) $("#list").insertAdjacentHTML("beforeend", `<div class="more">Loading more…</div>`);
    if (!state.more && marker) marker.remove();
  } else renderList();
}

/// The next page, when the list is scrolled near its end. The request
/// overlaps the rows already loaded, because articles marked read in Unread
/// have left the scope on the server; duplicates are dropped here.
///
/// No cap on the total: a single request is capped, pages are not, and a stop
/// at 20000 left "Loading more…" showing with older articles out of reach.
async function loadMore() {
  const scope = listScope();
  if (!scope || !state.more || state.loadingMore) return;
  // Only the list on screen pages. Between a feed click or a search and its
  // reply, the rows here belong to the previous list, and their count taken
  // as an offset into the new one skipped rows or mixed the two.
  const key = listKeyNow();
  if (key !== state.listKey) return;
  state.loadingMore = true;
  const ticket = loadListTicket;
  // Where the next page starts on the server: the loaded rows still in the
  // scope. Articles read in Unread (or unstarred in Starred) have left it
  // there but not here, and counting them skipped as many unread articles.
  const still = scope === "unread" ? state.items.filter((i) => !i.read).length
    : scope === "starred" ? state.items.filter((i) => i.starred).length
    : state.items.length;
  const overlap = Math.min(50, still);
  try {
    const page = await fetchList(scope, still - overlap, PAGE + overlap);
    if (ticket !== loadListTicket || key !== state.listKey || key !== listKeyNow()) return;
    const have = new Set(state.items.map((i) => i.id));
    const added = page.filter((i) => !have.has(i.id));
    state.items = state.items.concat(added);
    state.more = page.length === PAGE + overlap;
    appendRows(added);
  } finally {
    state.loadingMore = false;
  }
}

function setSort(sort) {
  state.sort = sort;
  try { localStorage.setItem("sort", sort); } catch {}
  paintSortHead();
  $("#list").scrollTop = 0;
  loadList();
}

/// A click on a column heading sorts by it; a second click reverses.
/// Dates start newest first, text starts A to Z.
function toggleSort(key) {
  const cur = state.sort.replace(/^-/, "");
  if (cur === key) setSort(state.sort.startsWith("-") ? key : `-${key}`);
  else setSort(key === "date" ? "-date" : key);
}

function paintSortHead() {
  const key = state.sort.replace(/^-/, "");
  const arrow = state.sort.startsWith("-") ? " ▼" : " ▲";
  document.querySelectorAll("#listhead [data-sort]").forEach((b) => {
    const on = b.dataset.sort === key;
    b.setAttribute("aria-sort", on ? (state.sort.startsWith("-") ? "descending" : "ascending") : "none");
    b.textContent = b.dataset.label + (on ? arrow : "");
  });
}

/// The chips a row shows for its labels. In compact density there is no room
/// for words, so they collapse to coloured squares that still carry the name
/// as a tooltip.
function labelChips(ids) {
  if (!ids?.length || !state.labels.length) return "";
  const dense = document.documentElement.dataset.density === "compact";
  const chips = ids
    .map((id) => state.labels.find((l) => l.id === id))
    .filter(Boolean)
    .map((l) => {
      const bg = l.color_bg || "var(--ink-4)";
      const fg = l.color_text || "#fff";
      return dense
        ? `<span class="lchip dotonly" data-bg="${esc(bg)}" title="${esc(l.name)}"></span>`
        : `<span class="lchip" data-bg="${esc(bg)}" data-fg="${esc(fg)}">${esc(l.name)}</span>`;
    })
    .join("");
  return chips ? `<span class="lchips">${chips}</span>` : "";
}

function marksHtml(i) {
  return `<div class="marks">
    <button class="star${i.starred ? " on" : ""}" data-star="${i.id}"
            aria-label="${i.starred ? "Unstar" : "Star"}">${i.starred ? STAR_SOLID : STAR_OUTLINE}</button>
    <button class="dot" data-dot="${i.id}" aria-pressed="${!i.read}"
            aria-label="Mark as ${i.read ? "unread" : "read"}"
            title="Mark as ${i.read ? "unread" : "read"}">${DOT}</button>
  </div>`;
}

function metaHtml(i) {
  return `<div class="m">
    ${feedIcon(i.feed_id, i.feed_title, "tiny feed")}
    <span class="feed">${esc(i.feed_title)}</span>
    <span class="feed faint">·</span>
    <span>${when(i.published)}</span>
    ${labelChips(i.labels)}
  </div>`;
}

/// What a row is grouped under, for the headings between rows. They follow
/// the sort: days for date, the feed or author for those, none for title,
/// where they would be one per row.
function listGrouper() {
  const sortKey = state.sort.replace(/^-/, "");
  return sortKey === "date" ? (i) => dayKey(i.published)
    : sortKey === "feed" ? (i) => i.feed_title || "?"
    : sortKey === "author" ? (i) => i.author || "No author"
    : null;
}

function rowHtml(i, paper) {
  if (paper) {
    // The focused card carries the whole article; the rest show an excerpt.
    // state.current is filled in by openArticle, so a card is "loading"
    // between the click and the fetch returning.
    const open = state.selected === i.id;
    const body = open
      ? (state.current
          ? `<div class="full"><div class="prose" data-prose="${i.id}"></div></div>`
          : `<div class="ex">${state.failed === i.id ? "Could not load this article." : "Loading…"}</div>`)
      : (i.excerpt ? `<div class="ex">${esc(i.excerpt)}</div>` : "");
    return `<div class="item${i.read ? " read" : ""}" data-id="${i.id}"
                  aria-selected="${open}" data-sel="${state.sel.has(i.id)}">
      <div class="head">
        ${marksHtml(i)}
        <div class="body">
          <div class="t">${esc(i.title)}</div>
          ${metaHtml(i)}
        </div>
      </div>
      ${body}
      ${open ? `<div class="cardbar">
        <button class="pill" data-open="${i.id}"${i.link ? "" : " disabled"}>Open in browser</button>
        <button class="pill" data-collapse="${i.id}">Collapse</button>
      </div>` : ""}
    </div>`;
  }
  return `<div class="item${i.read ? " read" : ""}" data-id="${i.id}"
                aria-selected="${state.selected === i.id}"
                data-sel="${state.sel.has(i.id)}">
    ${marksHtml(i)}
    <div class="body">
      <div class="t">${esc(i.title)}</div>
      ${metaHtml(i)}
    </div>
  </div>`;
}

/// Rows and their headings, continuing from the heading `group`.
function rowsHtml(items, group) {
  const paper = document.documentElement.dataset.layout === "newspaper";
  const groupOf = listGrouper();
  let html = "";
  for (const i of items) {
    if (groupOf) {
      const g = groupOf(i);
      if (g !== group) { group = g; html += `<div class="group">${esc(g.toUpperCase())}</div>`; }
    }
    html += rowHtml(i, paper);
  }
  return html;
}

/// The row element for each article id on screen. Built once per full draw
/// and kept up to date by the partial ones, so finding a row does not mean
/// searching every row, which made select-all quadratic.
let rowNodes = null;
/// The layout the rows on screen were drawn for: cards and plain rows are
/// different markup, so a reload after switching must draw them all again.
let drawnLayout = null;
function rowNode(id) {
  if (!rowNodes) {
    rowNodes = new Map();
    $("#list").querySelectorAll(".item").forEach((n) => rowNodes.set(Number(n.dataset.id), n));
  }
  return rowNodes.get(id);
}

/// The open card in the newspaper layout holds the article itself.
function fillOpenCard(el) {
  if (document.documentElement.dataset.layout !== "newspaper" || !state.current) return;
  const host = el.querySelector(`[data-prose="${state.current.id}"]`);
  if (!host) return;
  // Sanitised server-side by ammonia, same as the reading pane.
  host.innerHTML = state.current.html;
  host.querySelectorAll("a[href]").forEach((link) => {
    link.onclick = (e) => { e.preventDefault(); openUrl(link.href); };
  });
}

/// Draw the whole list. Only for a different list (another feed, a reload,
/// a new sort or layout): a change to a few rows goes through redrawRows, and
/// a new page through appendRows. Rebuilding every loaded row on each J or K
/// got slower the further down the list had been scrolled.
function renderList() {
  const el = $("#list");
  const q = state.query.trim().toLowerCase();
  const items = visibleItems();
  rowNodes = null;
  shownSel = new Set(state.sel);
  drawnLayout = document.documentElement.dataset.layout;

  if (!items.length) {
    el.innerHTML = `<div class="empty">${
      !listScope() ? "Select a feed." : q ? "Nothing matches." : "Nothing here."}</div>`;
    renderSelection();
    return;
  }

  let html = rowsHtml(items, null);
  if (state.more) html += `<div class="more">Loading more…</div>`;
  el.innerHTML = html;
  paint(el);
  fillOpenCard(el);
  renderSelection();
}

/// Draw these articles' rows again, leaving the rest of the list alone. For
/// changes that cannot move a row or its heading: read, starred, opened,
/// labelled.
function redrawRows(ids) {
  const el = $("#list");
  const paper = document.documentElement.dataset.layout === "newspaper";
  for (const id of new Set(ids)) {
    if (id === null || id === undefined) continue;
    const node = rowNode(id);
    const item = state.items.find((i) => i.id === id);
    if (!node || !item) continue;
    const t = document.createElement("template");
    t.innerHTML = rowHtml(item, paper);
    const fresh = t.content.firstElementChild;
    paint(t.content);
    node.replaceWith(fresh);
    rowNodes.set(id, fresh);
    if (paper && state.current?.id === id) fillOpenCard(el);
  }
}

/// Add a page of articles below the ones already drawn.
function appendRows(added) {
  const el = $("#list");
  if (!el.querySelector(".item")) { renderList(); return; }
  el.querySelector(".more")?.remove();
  if (added.length) {
    const before = state.items[state.items.length - added.length - 1];
    const groupOf = listGrouper();
    const t = document.createElement("template");
    t.innerHTML = rowsHtml(added, before && groupOf ? groupOf(before) : null);
    paint(t.content);
    if (rowNodes) t.content.querySelectorAll(".item").forEach((n) => rowNodes.set(Number(n.dataset.id), n));
    el.append(t.content);
  }
  if (state.more) el.insertAdjacentHTML("beforeend", `<div class="more">Loading more…</div>`);
}

/// Put each row's feed icon in line with state.icons, touching nothing else.
/// Icons are looked up in the background, a few a minute, and redrawing the
/// whole list for each batch froze the window with thousands of rows loaded.
function refreshRowIcons() {
  for (const i of state.items) {
    const icon = rowNode(i.id)?.querySelector(".m .favicon");
    if (!icon) continue;
    const want = state.icons[i.feed_id] || null;
    if ((icon.tagName === "IMG" ? icon.getAttribute("src") : null) === want) continue;
    const t = document.createElement("template");
    t.innerHTML = feedIcon(i.feed_id, i.feed_title, "tiny feed");
    paint(t.content);
    icon.replaceWith(t.content);
  }
}

function collapseCard() {
  const was = state.selected;
  state.selected = null;
  state.current = null;
  redrawRows([was]);
}

/// Clicks anywhere in the list, bound once. Handlers set on each row were
/// set again on every row whenever the list was drawn.
function onListClick(e) {
  const el = $("#list");
  const open = e.target.closest("[data-open]");
  if (open) {
    e.stopPropagation();
    const a = state.items.find((x) => x.id === Number(open.dataset.open));
    if (a?.link) openUrl(a.link);
    return;
  }
  if (e.target.closest("[data-collapse]")) {
    e.stopPropagation();
    collapseCard();
    return;
  }
  const star = e.target.closest("[data-star]");
  if (star) {
    e.stopPropagation();
    toggleRowStar(Number(star.dataset.star));
    return;
  }
  const dot = e.target.closest("[data-dot]");
  if (dot) {
    e.stopPropagation();
    toggleRowRead(Number(dot.dataset.dot));
    return;
  }
  const n = e.target.closest(".item");
  if (!n || !el.contains(n)) return;
  const paper = document.documentElement.dataset.layout === "newspaper";
  // In newspaper the article itself is inside the card, so a click on the
  // text, a link or the card's own buttons is not a click on the card.
  if (e.target.closest(".full, .cardbar")) return;
    const id = Number(n.dataset.id);

    if (e.shiftKey && state.anchor !== null) {
      // Range from the anchor to here, in the order the list is showing.
      const visible = visibleItems().map((i) => i.id);
      const a = visible.indexOf(state.anchor);
      const b = visible.indexOf(id);
      if (a >= 0 && b >= 0) {
        const [lo, hi] = a < b ? [a, b] : [b, a];
        setSelection(visible.slice(lo, hi + 1));
        // Shift-click extends the selection without moving the anchor, so a
        // second shift-click re-ranges from the same start.
        openArticle(id, { keepSelection: true });
        return;
      }
    }

    if (e.ctrlKey || e.metaKey) {
      // Toggle one row without disturbing the reading pane.
      const next = new Set(state.sel);
      next.has(id) ? next.delete(id) : next.add(id);
      setSelection(next, { anchor: id });
      return;
    }

    // A plain click on the open card's headline closes it again. Not the
    // second click of a double-click: that one opens the article in the
    // browser, and closing the card underneath it would be a surprise.
    if (paper && state.selected === id && e.detail <= 1) {
      collapseCard();
      return;
    }

    setSelection([id], { anchor: id });
    openArticle(id, { keepSelection: true });
}

async function toggleRowStar(id) {
  const it = state.items.find((x) => x.id === id);
  if (!it) return;
  it.starred = !it.starred;
  await invoke("set_starred", { ids: [it.id], starred: it.starred });
  if (state.selected === it.id) setStarButton($("#btn-star2"), it.starred);
  redrawRows([it.id]);
  loadTree();
}

async function toggleRowRead(id) {
  const it = state.items.find((x) => x.id === id);
  if (!it) return;
  it.read = !it.read;
  await invoke("set_read", { ids: [it.id], read: it.read });
  redrawRows([it.id]);
  loadTree();
  refreshStatus();
}

$("#list").onclick = onListClick;
$("#list").ondblclick = (e) => {
  const n = e.target.closest(".item");
  if (!n) return;
  const a = state.items.find((x) => x.id === Number(n.dataset.id));
  if (a?.link) openUrl(a.link);
};

// --------------------------------------------------------------- reading pane
async function openArticle(id, { keepSelection = false } = {}) {
  const was = state.selected;
  state.selected = id;
  state.current = null;
  state.failed = null;
  // Arriving by keyboard or by "next article" collapses the selection to the
  // one being read; a click has already set it.
  if (!keepSelection) { state.sel = new Set([id]); state.anchor = id; }
  redrawRows([was, id]);
  renderSelection();
  $("#article").innerHTML = '<div class="empty">Loading…</div>';

  let a;
  try {
    a = await invoke("article", { id });
  } catch (e) {
    if (state.selected !== id) return;
    $("#article").innerHTML = `<div class="empty">Could not load this article.<br><small>${esc(e)}</small></div>`;
    // In newspaper the reading pane is hidden; the card says it instead of
    // "Loading…" for ever.
    state.failed = id;
    if (document.documentElement.dataset.layout === "newspaper") redrawRows([id]);
    return;
  }
  state.failed = null;
  // Another article was opened while this one loaded. Drawing this one now
  // put the wrong article under the new selection.
  if (state.selected !== id) return;

  $("#article").innerHTML = `
   <div class="col">
    <div class="kicker">
      <span class="src">${esc(a.feed_title)}</span>
      <span class="faint">·</span>
      <span class="when">${a.published ? new Date(a.published).toLocaleString() : ""}</span>
      ${a.from_feed ? '<span class="chip">summary only</span>' : ""}
    </div>
    <h1 class="headline">${esc(a.title)}</h1>
    <div class="byline">${a.byline ? esc(a.byline) + " · " : ""}${a.read_minutes} min read</div>
    <div class="prose"></div>
   </div>`;

  // a.html is sanitised server-side by ammonia against an allowlist.
  $("#article .prose").innerHTML = a.html;

  // Links inside an article go to the real browser, never the webview.
  $("#article").querySelectorAll("a[href]").forEach((link) => {
    link.onclick = (e) => { e.preventDefault(); openUrl(link.href); };
  });

  $("#article").scrollTop = 0;
  // A summary shown while the full article is still being fetched says so,
  // rather than looking like that is all there is.
  if (a.pending) {
    $("#article .kicker")?.insertAdjacentHTML("beforeend",
      '<span class="chip pendingchip">fetching full article…</span>');
  }
  $("#openext").disabled = !a.link;
  $("#openext").dataset.href = a.link || "";
  state.current = a;
  setStarButton($("#btn-star2"), a.starred);
  // In newspaper the article is drawn inside its card, which only exists once
  // state.current is filled in.
  if (document.documentElement.dataset.layout === "newspaper") {
    redrawRows([id]);
    // Not every engine has scrollIntoView, and a missing scroll is a cosmetic
    // loss rather than a reason to throw out of the click handler.
    const card = rowNode(id);
    card?.scrollIntoView?.({ block: "nearest" });
  }

  const it = state.items.find((i) => i.id === id);
  if (it && !it.read && settingOn("reading.mark_read_on_open")) {
    it.read = true;
    await invoke("set_read", { ids: [id], read: true });
    redrawRows([id]);
    loadTree();
    refreshStatus();
  }
}

/// The rows the list is showing. Select-all and next/previous work on these.
function visibleItems() {
  // The search runs in the database now, so everything loaded matches.
  return state.items;
}

function selectAllVisible() {
  setSelection(visibleItems().map((i) => i.id));
}

async function step(delta) {
  let items = visibleItems();
  if (!items.length) return;
  const idx = items.findIndex((i) => i.id === state.selected);
  // The focused article can be gone from the list entirely; start from the
  // end we are heading towards rather than off by one.
  let next = idx < 0
    ? items[delta > 0 ? 0 : items.length - 1]
    : items[idx + delta];
  // The last loaded row is not the last article: fetch the next page and go on.
  if (!next && idx >= 0 && state.more) {
    const from = state.selected;
    await loadMore();
    if (state.selected !== from) return;
    items = visibleItems();
    next = items[items.findIndex((i) => i.id === from) + delta];
  }
  if (!next) return;
  // J and K are about articles now. Focus left on a clicked feed made the
  // next Delete remove that feed.
  if (document.activeElement?.closest?.("#tree")) $("#list").focus({ preventScroll: true });
  openArticle(next.id);
  // openArticle has drawn the row by now. Not every engine has scrollIntoView.
  rowNode(next.id)?.scrollIntoView?.({ block: "nearest" });
}

// -------------------------------------------------------------------- actions

/// Spinner on the button, a progress bar under the toolbar, and the name of
/// the feed being fetched in the status line. Without this an update looks
/// like nothing happening followed by a number.
function setUpdating(on, p) {
  const b = $("#btn-update");
  b.classList.toggle("busy", on);
  b.querySelector("svg").classList.toggle("spin", on);
  b.disabled = on;

  const bar = $("#progress");
  bar.classList.toggle("show", on);
  if (p && p.total) {
    bar.style.setProperty("--pct", Math.round((p.done / p.total) * 100) + "%");
  }

  const msg = $("#st-msg");
  if (!on) {
    msg.textContent = "";
    return;
  }
  const text = p && p.total
    ? `Updating ${p.done}/${p.total}${p.title ? " · " + p.title : ""}`
    : "Updating…";
  msg.innerHTML = '<span class="dotpulse"></span>';
  msg.append(text);
}

let manualUpdate = false;
$("#btn-update").onclick = async () => {
  if (manualUpdate) return;
  manualUpdate = true;
  setUpdating(true, { done: 0, total: 1, title: "" });
  try {
    const r = await invoke("update_all");
    toast(r.attempted === 0
      ? "Nothing due yet"
      : `${r.new_articles} new · ${r.not_modified} unchanged${r.failed ? ` · ${r.failed} failed` : ""}`);
    await loadTree();
    await loadList();
    await refreshStatus();
  } catch (e) {
    toast(String(e));
  } finally {
    manualUpdate = false;
    setUpdating(false);
  }
};

const addFeed = async () => {
  const address = await ask({ title: "Add a feed", placeholder: "Site or feed address, such as example.com",
                              confirmLabel: "Add" });
  if (!address) return;
  // A site's address works as well as its feed's: the page is asked which
  // feeds it has.
  toast("Looking for the feed…");
  let found;
  try { found = await invoke("discover_feed", { address }); }
  catch (e) {
    // The address could not be checked: it asks for a password, the server
    // is down, or it turns the checker away. It can still be added as it
    // is; a feed that needs signing in then opens its properties for that.
    const typed = address.includes("://") ? address.trim() : `https://${address.trim()}`;
    const needsSignIn = String(e).includes("http 401");
    if (!needsSignIn) {
      const go = await ask({ title: `Could not check ${typed}. Add it anyway?`, placeholder: null,
                             confirmLabel: "Add anyway" });
      if (!go) return;
    }
    found = [{ url: typed, title: null, subscribed: false, signIn: needsSignIn }];
  }
  if (!found.length) return toast("No feed found at that address");
  if (!found.some((f) => !f.subscribed)) return toast("You are already subscribed to that feed");
  let url = found.find((f) => !f.subscribed).url;
  if (found.length > 1) {
    url = await choose({
      title: "This site has more than one feed",
      options: found.map((f) => ({
        label: f.title || f.url, sub: f.title ? f.url : "", value: f.url,
        disabled: f.subscribed, note: f.subscribed ? "subscribed" : "",
      })),
    });
    if (!url) return;
  }
  try {
    const id = await invoke("add_feed", { url, parentId: null });
    await loadTree();
    if (found.find((f) => f.url === url)?.signIn) {
      toast("This feed needs a user name and password");
      return openFeedSettings(id);
    }
    toast("Added, fetching…");
    const r = await invoke("update_feed_now", { id });
    await loadTree();
    await loadList();
    // The icon, now that the first fetch has told us where the site is.
    invoke("refresh_feed_icon", { id })
      .then(async (found) => { if (found) { await loadIcons(); await loadTree(); refreshRowIcons(); } })
      .catch(() => {});
    toast(r.failed ? "Added, but the first fetch failed" : `Added, ${r.new_articles} articles`);
  } catch (e) { toast(String(e)); }
};
$("#btn-addfeed").onclick = addFeed;
$("#btn-addfeed2").onclick = addFeed;

$("#btn-addfolder").onclick = async () => {
  const name = await ask({ title: "New folder", placeholder: "Folder name" });
  if (!name) return;
  await invoke("add_folder", { name, parentId: null });
  loadTree();
};

$("#btn-refreshone").onclick = async () => {
  const cur = document.querySelector('.node[aria-current="true"]');
  if (!cur?.dataset.id) return toast("Select a feed first");
  const r = await invoke("update_feed_now", { id: Number(cur.dataset.id) });
  toast(r.failed ? "Update failed" : r.not_modified ? "No change" : `${r.new_articles} new`);
  loadTree(); loadList();
};

$("#btn-removefeed").onclick = () => removeNode(document.querySelector('.node[aria-current="true"]'));

/// After a rename: redraw the tree, and the list's header when the renamed
/// node is the one being shown.
async function renamed(id, name) {
  const cur = document.querySelector('.node[aria-current="true"]');
  if (cur && Number(cur.dataset.id) === id) {
    state.scopeName = name;
    $("#scopename").textContent = name;
  }
  await loadTree();
}

async function removeNode(cur) {
  if (!cur?.dataset.id) return toast("Select a feed first");
  const folder = cur.dataset.scope?.startsWith("folder:");
  const ok = await ask({
    title: folder
      ? `Remove the folder "${cur.dataset.label}", every feed in it and their articles?`
      : `Remove "${cur.dataset.label}" and its articles?`,
    placeholder: null,
    confirmLabel: "Remove",
    danger: true,
  });
  if (!ok) return;
  await invoke("remove_feed", { id: Number(cur.dataset.id) });
  await clearScope();
  loadTree();
};

// Bulk actions use the "if any are off, turn them all on" rule, which is what
// every mail client does and is the only reading that stays predictable when
// a mixed set is selected.
const toggleStar = async ({ onlyShown = false } = {}) => {
  const ids = onlyShown ? (state.selected ? [state.selected] : []) : targetIds();
  if (!ids.length) return;
  const rows = itemsFor(ids);
  // The focused article may no longer be in the list, in which case the
  // reading pane's own copy is the only state we have.
  const known = rows.length ? rows : (state.current ? [state.current] : []);
  if (!known.length) return;

  const starred = !known.every((i) => i.starred);
  await invoke("set_starred", { ids, starred });
  rows.forEach((i) => { i.starred = starred; });
  if (state.current && ids.includes(state.current.id)) state.current.starred = starred;
  if (ids.includes(state.selected)) setStarButton($("#btn-star2"), starred);
  redrawRows(ids); loadTree();
};
$("#btn-star").onclick = () => toggleStar();
// The reading pane's star is about the article on screen, whatever else is
// selected in the list.
$("#btn-star2").onclick = () => toggleStar({ onlyShown: true });

$("#btn-toggleread").onclick = async () => {
  const ids = targetIds();
  if (!ids.length) return;
  const rows = itemsFor(ids);
  // Nothing in the list to read state from: assume the focused article was
  // marked read on open and flip it back.
  const read = rows.length ? !rows.every((i) => i.read) : false;
  await invoke("set_read", { ids, read });
  rows.forEach((i) => { i.read = read; });
  redrawRows(ids); loadTree(); refreshStatus();
};

$("#btn-delete").onclick = () => deleteArticles(targetIds());

async function deleteArticles(ids) {
  if (!ids.length) return;
  const restoring = state.scope === "deleted";
  await invoke("set_deleted", { ids, deleted: !restoring });
  pushUndo(
    restoring ? `restore ${ids.length}` : `delete ${ids.length}`,
    () => invoke("set_deleted", { ids, deleted: restoring }),
  );
  if (ids.includes(state.selected)) {
    state.selected = null;
    state.current = null;
    $("#article").innerHTML = '<div class="empty">Select an article.</div>';
  }
  setSelection([]);
  toast(ids.length > 1
    ? `${restoring ? "Restored" : "Deleted"} ${ids.length} articles`
    : restoring ? "Restored" : "Deleted");
  await loadList(); await loadTree(); await refreshStatus();
}

$("#btn-listmarkall").onclick = async () => {
  if (!state.scope) return toast("Select a feed first");
  // The backend returns every id it marked, including ones beyond the 500
  // the list loads, so undo puts all of them back.
  const wasUnread = await invoke("mark_scope_read", { scope: state.scope });
  const unread = wasUnread.length;
  if (!unread) return toast("Nothing unread here");
  pushUndo(`mark ${unread} read`,
           () => invoke("set_read", { ids: wasUnread, read: false }));
  await loadList(); await loadTree(); await refreshStatus();
  toast(`Marked ${unread} as read`);
};

$("#btn-prev").onclick = () => step(-1);
$("#btn-next").onclick = () => step(1);
$("#openext").onclick = () => { const h = $("#openext").dataset.href; if (h) openUrl(h); };

$("#btn-import").onclick = async () => {
  let picked;
  try {
    picked = await dialog.open({
      title: "Import",
      multiple: false,
      directory: false,
      filters: [
        { name: "Subscriptions", extensions: ["opml", "xml", "db", "sqlite", "sqlite3"] },
        { name: "OPML", extensions: ["opml", "xml"] },
        { name: "QuiteRSS database", extensions: ["db", "sqlite", "sqlite3"] },
        { name: "All files", extensions: ["*"] },
      ],
    });
  } catch (e) {
    return toast("Could not open the file picker: " + e);
  }
  if (!picked) return;
  // The plugin returns a string, an array, or an object depending on version.
  const path = Array.isArray(picked) ? picked[0] : (picked.path ?? picked);

  try {
    toast("Importing…");
    const r = await invoke("import_file", { path });
    const bits = [];
    if (r.feeds) bits.push(`${r.feeds} feed${r.feeds === 1 ? "" : "s"}`);
    if (r.folders) bits.push(`${r.folders} folder${r.folders === 1 ? "" : "s"}`);
    if (r.news) bits.push(`${r.news} articles`);
    if (r.duplicates) bits.push(`${r.duplicates} already subscribed`);
    toast(bits.length ? "Imported " + bits.join(", ") : "Nothing new to import");
    if (r.skipped.length) console.warn("import skipped:", r.skipped);
    await loadTree();
    await loadList();
    await refreshStatus();
  } catch (e) { toast(String(e)); }
};

$("#btn-export").onclick = async () => {
  let path;
  try {
    path = await dialog.save({
      title: "Export OPML",
      defaultPath: "snaprss.opml",
      filters: [{ name: "OPML", extensions: ["opml"] }],
    });
  } catch (e) {
    return toast("Could not open the save dialog: " + e);
  }
  if (!path) return;
  try {
    const n = await invoke("export_opml", { path });
    toast(`Exported ${n} feed${n === 1 ? "" : "s"}`);
  } catch (e) { toast(String(e)); }
};

// ------------------------------------------------------------------- undo
//
// Only reversible things go on the stack. Deleting an article is a soft delete
// and comes straight back; unsubscribing a feed is a cascade and cannot be
// undone, so it asks first instead of pretending Ctrl+Z will save you.

const undoStack = [];

function pushUndo(label, run) {
  undoStack.push({ label, run });
  if (undoStack.length > 30) undoStack.shift();
}

async function undo() {
  const u = undoStack.pop();
  if (!u) return toast("Nothing to undo");
  try {
    await u.run();
    toast("Undone: " + u.label);
    await loadList(); await loadTree(); await refreshStatus();
  } catch (e) {
    toast(String(e));
  }
}

// ---------------------------------------------------------------- context menu
//
// The webview's own menu is Back / Forward / Reload / Inspect Element, which is
// meaningless in an app with no navigation. It is suppressed everywhere except
// text fields, where the native cut/copy/paste is genuinely wanted.

/// Open menus, outermost first. A submenu pushes onto the stack and is popped
/// when the pointer moves back to a shallower level.
let ctxStack = [];

function closeCtx(depth = 0) {
  while (ctxStack.length > depth) ctxStack.pop().remove();
}

/// items: [{ label, run, disabled, danger, checked, items }] or the string "-".
/// An entry with its own `items` opens a submenu on hover.
function showCtx(x, y, items, heading, depth = 0) {
  closeCtx(depth);
  const el = document.createElement("div");
  el.className = "ctx";
  el.setAttribute("role", "menu");
  el.dataset.depth = String(depth);

  if (heading) {
    const h = document.createElement("div");
    h.className = "ctx-head";
    h.textContent = heading;
    el.append(h);
  }

  for (const it of items) {
    if (it === "-") {
      const sep = document.createElement("div");
      sep.className = "ctx-sep";
      el.append(sep);
      continue;
    }
    const b = document.createElement("button");
    b.type = "button";
    b.setAttribute("role", "menuitem");
    if (it.danger) b.classList.add("danger");
    if (it.disabled) b.disabled = true;

    const sub = typeof it.items === "function" ? null : it.items;
    const hasSub = !!(it.items);
    b.innerHTML =
      `<span class="tick">${it.checked
        ? '<svg width="13" height="13" viewBox="0 0 16 16" fill="none"><path d="M3 8.4l3 3 7-7" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>'
        : ""}</span><span class="ctx-label">${esc(it.label)}</span>` +
      (it.hint ? `<span class="ctx-hint">${esc(it.hint)}</span>` : "") +
      (hasSub
        ? '<svg class="ctx-arrow" width="12" height="12" viewBox="0 0 16 16" fill="none"><path d="M6.5 4L10.5 8l-4 4" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/></svg>'
        : "");

    if (hasSub) {
      const open = () => {
        if (b.disabled) return;
        const r = b.getBoundingClientRect();
        const list = typeof it.items === "function" ? it.items() : sub;
        showCtx(r.right - 4, r.top - 5, list, null, depth + 1);
      };
      b.onmouseenter = open;
      b.onclick = open;
      b.onfocus = open;
    } else {
      // Anything shallower than this item is stale once the pointer is here.
      b.onmouseenter = () => closeCtx(depth + 1);
      b.onclick = () => { closeCtx(); it.run?.(); };
    }
    el.append(b);
  }

  document.body.append(el);

  const r = el.getBoundingClientRect();
  let left = x;
  // A submenu that will not fit flips to the left of its parent.
  if (left + r.width > window.innerWidth - 8)
    left = depth > 0 ? Math.max(8, x - r.width - 20) : window.innerWidth - r.width - 8;
  el.style.left = Math.max(8, left) + "px";
  el.style.top = Math.max(8, Math.min(y, window.innerHeight - r.height - 8)) + "px";
  ctxStack.push(el);
  return el;
}

addEventListener("click", (e) => { if (!e.target.closest(".ctx")) closeCtx(); }, true);
addEventListener("keydown", (e) => { if (e.key === "Escape") closeCtx(); }, true);
// Wrapped: passed directly, the Event arrived as closeCtx's `depth` argument
// and nothing closed.
addEventListener("scroll", () => closeCtx(), true);
addEventListener("resize", () => closeCtx());
addEventListener("blur", () => closeCtx());

async function copyText(text, what = "Copied") {
  if (!text) return;
  try {
    await clipboard.writeText(text);
    toast(what);
  } catch (e) {
    toast("Could not copy: " + e);
  }
}

document.addEventListener("contextmenu", async (e) => {
  // Leave text fields alone so the native menu keeps working there.
  if (e.target.closest("input, textarea, [contenteditable]")) return;
  e.preventDefault();

  const { clientX: x, clientY: y } = e;

  // ---- feed tree ----------------------------------------------------------
  const node = e.target.closest(".node");
  if (node) {
    const scope = node.dataset.scope;
    const id = node.dataset.id ? Number(node.dataset.id) : null;
    const label = node.dataset.label;
    selectScope(scope, label);

    if (scope.startsWith("label:")) {
      const lid = Number(scope.slice(6));
      return showCtx(x, y, [
        { label: "Mark all as read", run: async () => {
            await invoke("mark_scope_read", { scope });
            await loadList(); await loadTree(); await refreshStatus();
          } },
        "-",
        { label: "Edit label…", run: () => editLabelById(lid) },
        { label: "Delete label…", danger: true, run: () => deleteLabelById(lid) },
        "-",
        { label: "Manage labels…", run: () => openSettings("labels") },
      ], label);
    }

    if (id === null) {
      // A smart folder: Unread, All, Starred, Deleted. Emptying belongs on the
      // Deleted folder itself, not only in Tools and Settings.
      if (scope === "deleted") {
        return showCtx(x, y, [
          { label: "Empty Deleted…", danger: true, run: () => emptyTrash() },
        ], label);
      }
      return showCtx(x, y, [
        { label: "Mark all as read", run: async () => {
            await invoke("mark_scope_read", { scope });
            await loadList(); await loadTree(); await refreshStatus();
          } },
      ], label);
    }

    const isFolder = scope.startsWith("folder:");
    const feed = findNode(state.tree, id);
    const items = [];

    {
      items.push({ label: isFolder ? "Update this folder" : "Update this feed", run: async () => {
        const r = await invoke("update_feed_now", { id });
        toast(r.failed ? "Update failed" : r.not_modified ? "No change" : `${r.new_articles} new`);
        await loadTree(); await loadList();
      }});
    }
    items.push({ label: isFolder ? "Mark folder as read" : "Mark feed as read", run: async () => {
      await invoke("mark_scope_read", { scope });
      await loadList(); await loadTree(); await refreshStatus();
    }});
    items.push("-");

    if (!isFolder) {
      const descOnly = feed?.reading_mode === 1;
      items.push({ label: "Read: full article", checked: !descOnly, run: async () => {
        await invoke("set_reading_mode", { id, descriptionOnly: false });
        await loadTree(); toast("Full article");
      }});
      items.push({ label: "Read: summary only", checked: descOnly, run: async () => {
        await invoke("set_reading_mode", { id, descriptionOnly: true });
        await loadTree(); toast("Summary only");
      }});
      items.push("-");
      items.push({ label: "Copy feed address", run: () => copyText(feed?.xml_url, "Feed address copied") });
      items.push({ label: "Open site in browser", disabled: !feed?.html_url,
                   run: () => openUrl(feed.html_url) });
      items.push("-");
    } else {
      items.push({ label: "New folder inside", run: async () => {
        const name = await ask({ title: "New folder", placeholder: "Folder name" });
        if (name) { await invoke("add_folder", { name, parentId: id }); loadTree(); }
      }});
    }

    if (!isFolder) {
      items.push({ label: "Properties…", run: () => openFeedSettings(id) });
    }
    items.push({ label: "Rename…", run: async () => {
      const name = await ask({ title: "Rename", placeholder: "Name", value: label });
      if (name) { await invoke("rename_node", { id, name }); renamed(id, name); }
    }});
    items.push({ label: isFolder ? "Delete folder and its feeds" : "Unsubscribe", danger: true,
      run: async () => {
        const ok = await ask({
          title: `Remove "${label}" and its articles?`,
          placeholder: null, confirmLabel: "Remove", danger: true,
        });
        if (!ok) return;
        await invoke("remove_feed", { id });
        await clearScope();
        await loadTree(); await refreshStatus();
      }});

    return showCtx(x, y, items, label);
  }

  // ---- article list -------------------------------------------------------
  const item = e.target.closest(".item");
  if (item) {
    const id = Number(item.dataset.id);
    const art = state.items.find((i) => i.id === id);
    if (!art) return;

    // Right-clicking inside a multi-selection acts on all of it; right-clicking
    // outside one collapses to the row under the cursor, as every file manager
    // does.
    if (!state.sel.has(id)) {
      setSelection([id], { anchor: id });
      if (state.selected !== id) openArticle(id, { keepSelection: true });
    }
    const ids = targetIds();
    const many = ids.length > 1;
    const rows = itemsFor(ids);
    const allStarred = rows.length > 0 && rows.every((i) => i.starred);
    const allRead = rows.length > 0 && rows.every((i) => i.read);

    if (many) {
      return showCtx(x, y, [
        { label: allStarred ? "Remove stars" : "Star all", run: async () => {
            await invoke("set_starred", { ids, starred: !allStarred });
            rows.forEach((i) => { i.starred = !allStarred; });
            redrawRows(ids); loadTree();
          }},
        { label: allRead ? "Mark as unread" : "Mark as read", run: async () => {
            await invoke("set_read", { ids, read: !allRead });
            rows.forEach((i) => { i.read = !allRead; });
            redrawRows(ids); loadTree(); refreshStatus();
          }},
        { label: "Labels", items: () => labelMenuItems() },
        "-",
        { label: "Select all", run: selectAllVisible },
        { label: "Clear selection", run: () => setSelection(state.selected ? [state.selected] : []) },
        "-",
        { label: state.scope === "deleted" ? "Restore" : "Delete",
          danger: state.scope !== "deleted",
          run: () => $("#btn-delete").click() },
      ], `${ids.length} articles selected`);
    }

    return showCtx(x, y, [
      { label: "Open in browser", disabled: !art.link, run: () => openUrl(art.link) },
      { label: "Copy link", disabled: !art.link, run: () => copyText(art.link, "Link copied") },
      { label: "Copy title", run: () => copyText(art.title, "Title copied") },
      "-",
      { label: art.starred ? "Remove star" : "Star", checked: art.starred, run: async () => {
          art.starred = !art.starred;
          await invoke("set_starred", { ids: [id], starred: art.starred });
          if (state.selected === id) setStarButton($("#btn-star2"), art.starred);
          redrawRows([id]); loadTree();
        }},
      { label: art.read ? "Mark as unread" : "Mark as read", run: async () => {
          art.read = !art.read;
          await invoke("set_read", { ids: [id], read: art.read });
          redrawRows([id]); loadTree(); refreshStatus();
        }},
      { label: "Labels", items: () => labelMenuItems() },
      "-",
      { label: "Select all", run: selectAllVisible },
      "-",
      { label: "Mark above as read", run: async () => {
          const shown = visibleItems();
          const idx = shown.findIndex((i) => i.id === id);
          const ids = shown.slice(0, idx + 1).filter((i) => !i.read).map((i) => i.id);
          if (!ids.length) return;
          await invoke("set_read", { ids, read: true });
          await loadList(); await loadTree(); await refreshStatus();
        }},
      "-",
      { label: state.scope === "deleted" ? "Restore" : "Delete", danger: state.scope !== "deleted",
        run: async () => {
          const del = state.scope !== "deleted";
          await invoke("set_deleted", { ids: [id], deleted: del });
          pushUndo(del ? "delete" : "restore",
                   () => invoke("set_deleted", { ids: [id], deleted: !del }));
          if (state.selected === id) {
            state.selected = null;
            $("#article").innerHTML = '<div class="empty">Select an article.</div>';
          }
          await loadList(); await loadTree(); await refreshStatus();
        }},
    ], art.title);
  }

  // ---- reading pane -------------------------------------------------------
  if (e.target.closest("#readpane")) {
    const sel = String(window.getSelection() || "").trim();
    const link = e.target.closest("a[href]")?.href;
    const img = e.target.closest("img")?.src;
    const art = state.items.find((i) => i.id === state.selected);

    return showCtx(x, y, [
      ...(link ? [
        { label: "Open link in browser", run: () => openUrl(link) },
        { label: "Copy link address", run: () => copyText(link, "Link copied") },
        "-",
      ] : []),
      ...(img ? [{ label: "Copy image address", run: () => copyText(img, "Image address copied") }, "-"] : []),
      { label: "Copy selection", disabled: !sel, run: () => copyText(sel, "Copied") },
      "-",
      { label: "Open article in browser", disabled: !art?.link, run: () => openUrl(art.link) },
      { label: "Copy article link", disabled: !art?.link, run: () => copyText(art.link, "Link copied") },
    ], art?.title);
  }

  // ---- anywhere else ------------------------------------------------------
  // Also the way back in when every toolbar has been customised away, so it
  // carries the whole app menu rather than a handful of shortcuts.
  showCtx(x, y, [
    { label: "Update all", run: () => $("#btn-update").click() },
    { label: "Add feed…", run: addFeed },
    { label: "New folder…", run: () => $("#btn-addfolder").click() },
    "-",
    { label: "Settings…", hint: keyHint("settings"), run: () => openSettings() },
    { label: "Menu", items: () => appMenu() },
  ]);
});

/// Depth-first lookup in the tree the backend returned.
function findNode(nodes, id) {
  for (const n of nodes) {
    if (n.id === id) return n;
    const hit = findNode(n.children || [], id);
    if (hit) return hit;
  }
  return null;
}

// ------------------------------------------------------------------ app menu
//
// The program-wide menu, in the top left. Grouped the way QuiteRSS groups it,
// because that is what anyone arriving from QuiteRSS will look for.

function appMenu() {
  const themeNames = {
    system: "System", system2: "System 2", dark: "Dark", gray: "Gray",
    green: "Green", orange: "Orange", pink: "Pink", purple: "Purple",
  };
  const theme = document.documentElement.dataset.theme || "system";
  const density = document.documentElement.dataset.density || "relaxed";
  const layout = document.documentElement.dataset.layout || "classic";
  const bars = toolbarConfig();

  return [
    { label: "Add", items: [
      { label: "Feed…", hint: keyHint("addFeed"), run: addFeed },
      { label: "Folder…", run: () => $("#btn-addfolder").click() },
    ]},
    "-",
    { label: "Import…", run: () => $("#btn-import").click() },
    { label: "Export OPML…", run: () => $("#btn-export").click() },
    { label: "Create backup…", run: backupDatabase },
    "-",
    { label: "View", items: [
      { label: "Theme", items: () =>
        Object.entries(themeNames).map(([k, name]) => ({
          label: name, checked: theme === k, run: () => applyTheme(k),
        })) },
      { label: "Density", items: [
        { label: "Relaxed", checked: density === "relaxed",
          run: () => setDensity("relaxed") },
        { label: "Compact", checked: density === "compact",
          run: () => setDensity("compact") },
      ]},
      { label: "Layout", items: [
        { label: "Classic", checked: layout === "classic", run: () => setLayout("classic") },
        { label: "Newspaper", checked: layout === "newspaper", run: () => setLayout("newspaper") },
      ]},
      { label: "Text size", items: [
        { label: "Larger", hint: keyHint("textBigger"), run: textBigger },
        { label: "Smaller", hint: keyHint("textSmaller"), run: textSmaller },
        { label: "Reset", hint: keyHint("textReset"), run: textReset },
      ]},
      { label: "Sort by", items: () => {
        const key = state.sort.replace(/^-/, "");
        const desc = state.sort.startsWith("-");
        return [
          ...[["date", "Date"], ["title", "Title"], ["author", "Author"], ["feed", "Feed"]].map(([k, name]) => ({
            label: name, checked: key === k,
            run: () => setSort(desc ? `-${k}` : k),
          })),
          "-",
          { label: "Ascending", checked: !desc, run: () => setSort(key) },
          { label: "Descending", checked: desc, run: () => setSort(`-${key}`) },
        ];
      }},
      "-",
      { label: "Toolbars", items: () => [
        ...Object.entries(TOOLBARS).map(([k, spec]) => ({
          label: spec.name, checked: bars[k].show,
          run: () => { const c = toolbarConfig(); c[k].show = !c[k].show; saveToolbars(c); },
        })),
        "-",
        { label: "Customise…", run: () => openSettings("toolbars") },
      ]},
      { label: "Categories panel", checked: $("#cats-head").getAttribute("aria-expanded") === "true",
        run: () => $("#cats-head").click() },
    ]},
    { label: "Feeds", items: [
      { label: "Update all", hint: keyHint("update"), run: () => $("#btn-update").click() },
      { label: "Update this feed", run: () => $("#btn-refreshone").click() },
      "-",
      { label: "Feed properties…", disabled: !currentFeedId(),
        run: () => openFeedSettings(currentFeedId()) },
    ]},
    { label: "News", items: [
      { label: "Mark all as read", hint: keyHint("markAllRead"), run: () => $("#btn-listmarkall").click() },
      { label: "Mark selected read", hint: keyHint("toggleRead"), run: () => $("#btn-toggleread").click() },
      { label: "Star selected", hint: keyHint("star"), run: () => $("#btn-star").click() },
      { label: "Labels", hint: keyHint("labels"), items: () => labelMenuItems() },
      "-",
      { label: "Select all", hint: keyHint("selectAll"), run: selectAllVisible },
      { label: "Delete selected", hint: keyHint("delete"), danger: true, run: () => $("#btn-delete").click() },
      { label: "Undo", hint: keyHint("undo"), disabled: !undoStack.length, run: undo },
    ]},
    { label: "Tools", items: [
      { label: "Settings…", hint: keyHint("settings"), run: () => openSettings() },
      { label: "Filters…", run: () => openSettings("filters") },
      { label: "Labels…", run: () => openSettings("labels") },
      "-",
      { label: "Clean up now", run: () => runCleanupNow() },
      { label: "Empty Deleted…", danger: true, run: () => emptyTrash() },
    ]},
    { label: "Help", items: [
      { label: "Keyboard shortcuts", run: () => openSettings("shortcuts") },
      { label: "Check for updates…", run: () => checkForUpdates() },
      { label: "About", run: () => openSettings("about") },
    ]},
    "-",
    { label: "Hide to tray", run: () => getCurrentWindow().hide() },
    { label: "Exit", hint: keyHint("quit"), run: () => invoke("quit_app") },
  ];
}

/// The feed the tree currently has selected, or null if it is a folder or a
/// smart category.
function currentFeedId() {
  const cur = document.querySelector('.node[aria-current="true"]');
  return cur?.dataset.id && cur.dataset.scope?.startsWith("feed:")
    ? Number(cur.dataset.id)
    : null;
}

$("#btn-appmenu").onclick = (e) => {
  e.stopPropagation();
  const r = (toolbarAnchor || $("#btn-appmenu")).getBoundingClientRect();
  showCtx(r.left, r.bottom + 4, appMenu());
};

function setDensity(d) {
  document.documentElement.dataset.density = d;
  localStorage.setItem("density", d);
  // Every copy, including clones in other toolbars.
  document.querySelectorAll("button[data-density]").forEach((x) =>
    x.setAttribute("aria-pressed", String(x.dataset.density === d)));
  // Label chips are drawn differently in compact.
  renderList();
}

async function backupDatabase() {
  let path;
  try {
    path = await dialog.save({
      title: "Back up",
      defaultPath: `snaprss-${new Date().toISOString().slice(0, 10)}.db`,
      filters: [{ name: "SQLite database", extensions: ["db"] }],
    });
  } catch (e) { return toast("Could not open the save dialog: " + e); }
  if (!path) return;
  try {
    const bytes = await invoke("backup_db", { path });
    toast(`Backed up ${fmtBytes(bytes)}`);
  } catch (e) { toast(String(e)); }
}

async function runCleanupNow() {
  try {
    const r = await invoke("run_cleanup", { feedId: null });
    const n = r.by_read + r.by_age + r.by_count;
    toast(n
      ? `Moved ${n} article${n === 1 ? "" : "s"} to Deleted`
      : "Nothing to clean up");
    if (n) { await loadTree(); await loadList(); await refreshStatus(); }
  } catch (e) { toast(String(e)); }
}

async function emptyTrash() {
  const stats = await invoke("db_stats").catch(() => null);
  if (!stats?.deleted) return toast("Deleted is already empty");
  const ok = await ask({
    title: `Permanently delete ${stats.deleted} article${stats.deleted === 1 ? "" : "s"}?`,
    placeholder: null, confirmLabel: "Delete", danger: true,
  });
  if (!ok) return;
  const r = await invoke("purge_deleted", { olderThanDays: null });
  toast(`Removed ${r.purged}`);
  await loadTree(); await loadList(); await refreshStatus();
}

function fmtBytes(n) {
  if (!n) return "0 B";
  const u = ["B", "KB", "MB", "GB"];
  const i = Math.min(u.length - 1, Math.floor(Math.log(n) / Math.log(1024)));
  return `${(n / 1024 ** i).toFixed(i ? 1 : 0)} ${u[i]}`;
}

// ------------------------------------------------------------------ settings
//
// One window, a page per topic. Everything global lives in the `settings`
// table; per-feed options are reached from the feed's own properties page,
// because a setting that silently applies to 200 feeds is a trap.

const THEME_NAMES = {
  system: "System", system2: "System 2", dark: "Dark", gray: "Gray",
  green: "Green", orange: "Orange", pink: "Pink", purple: "Purple",
};

/// Everything the settings window reads and writes, with its default. Keeping
/// the list here means adding an option is one line plus a control.
const SETTING_DEFAULTS = {
  "update.interval_minutes": "15",
  "update.on_startup": "1",
  "reading.mark_read_on_open": "1",
  "reading.default_description_only": "0",
  "reading.enter_opens_browser": "1",
  "cleanup.delete_read": "0",
  "cleanup.max_age_days": "",
  "cleanup.max_to_keep": "",
  "cleanup.never_delete_unread": "1",
  "cleanup.never_delete_starred": "1",
  "cleanup.never_delete_labeled": "1",
  "cleanup.purge_after_days": "30",
  "startup.minimized": "0",
  "startup.close_to_tray": "0",
  "startup.minimize_to_tray": "0",
  "tray.show_unread": "1",
  "updates.repo": "masterrite/SnapRSS",
  "updates.auto": "1",
};

let sheetEl = null;

function closeSheet() { sheetEl?.remove(); sheetEl = null; keyCapture = null; }

function sw(key, on) {
  return `<span class="sw" role="switch" tabindex="0" aria-checked="${!!on}" data-sw="${key}"><i></i></span>`;
}

function themeCard(k, name, active) {
  const c = THEME_SWATCH[k];
  return `<label class="theme-card" aria-checked="${active}" data-theme-pick="${k}">
    <svg width="100%" viewBox="0 0 176 80" class="swatchsvg">
      <rect width="176" height="80" fill="${c.page}"/>
      <rect width="176" height="13" fill="${c.chrome}"/>
      <rect x="6" y="4.5" width="22" height="4" rx="2" fill="${c.accent}"/>
      <rect y="13" width="44" height="67" fill="${c.side}"/>
      <rect x="44" y="13" width="56" height="67" fill="${c.list}"/>
      <rect x="6" y="20" width="26" height="3" rx="1.5" fill="${c.accent}"/>
      <rect x="6" y="28" width="30" height="3" rx="1.5" fill="${c.line}"/>
      <rect x="6" y="36" width="24" height="3" rx="1.5" fill="${c.line}"/>
      <rect x="50" y="20" width="44" height="3" rx="1.5" fill="${c.line}"/>
      <rect x="50" y="27" width="34" height="3" rx="1.5" fill="${c.line2}"/>
      <rect x="50" y="36" width="44" height="3" rx="1.5" fill="${c.line}"/>
      <rect x="50" y="43" width="30" height="3" rx="1.5" fill="${c.line2}"/>
      <rect x="106" y="20" width="52" height="5" rx="2.5" fill="${c.line}"/>
      <rect x="106" y="32" width="62" height="3" rx="1.5" fill="${c.line2}"/>
      <rect x="106" y="39" width="62" height="3" rx="1.5" fill="${c.line2}"/>
      <rect x="106" y="46" width="48" height="3" rx="1.5" fill="${c.line2}"/>
    </svg>
    <div class="name">${name}</div>
  </label>`;
}

const THEME_SWATCH = {
  system:  { chrome:"#E4E0D8", side:"#EDE9E2", list:"#F7F5F1", page:"#FFFFFF", accent:"#B8531D", line:"#C9C3B8", line2:"#DED8CD" },
  system2: { chrome:"#E3E5E9", side:"#EDEEF1", list:"#F6F7F9", page:"#FFFFFF", accent:"#2F5F91", line:"#C6CAD1", line2:"#DCE0E6" },
  dark:    { chrome:"#16191D", side:"#131619", list:"#181B1E", page:"#1A1D21", accent:"#E2703A", line:"#3A4048", line2:"#2A3036" },
  gray:    { chrome:"#CFCFCF", side:"#DCDCDC", list:"#EAEAEA", page:"#F6F6F6", accent:"#4A4A4A", line:"#BDBDBD", line2:"#D2D2D2" },
  green:   { chrome:"#D6E3CE", side:"#E2EBDC", list:"#EEF3EA", page:"#F8FBF5", accent:"#3F6B31", line:"#BCCCB3", line2:"#D2E0CA" },
  orange:  { chrome:"#F0DCC8", side:"#F6E7D8", list:"#FAF1E8", page:"#FEFAF6", accent:"#B4531C", line:"#DBC0A5", line2:"#EBD8C4" },
  pink:    { chrome:"#F0D5DE", side:"#F6E3EA", list:"#FAEFF3", page:"#FEF8FA", accent:"#9C3D61", line:"#DBB4C3", line2:"#EBD0DA" },
  purple:  { chrome:"#DDD4EC", side:"#E7E0F2", list:"#F2EEF8", page:"#FAF8FD", accent:"#5E4390", line:"#BFB2D6", line2:"#D6CCE8" },
};

/// Opening a sheet awaits the backend before drawing it. Two opens in flight
/// (Ctrl+, twice while an update held the database) drew two sheets, and the
/// first could not be closed by anything. Only the latest open draws.
let sheetTicket = 0;

async function openSettings(page = "general") {
  closeSheet();
  const ticket = ++sheetTicket;
  const values = { ...SETTING_DEFAULTS, ...(await invoke("get_settings").catch(() => ({}))) };
  // The login item lives in the OS, not the database, so it is read from there
  // and written straight through rather than waiting for Save.
  values._autostart = (await invoke("autostart_enabled").catch(() => false)) ? "1" : "0";
  if (ticket !== sheetTicket) return;
  closeSheet();
  const pending = { ...values };

  const back = document.createElement("div");
  back.className = "sheet-back";
  back.innerHTML = `
    <div class="sheet" role="dialog" aria-modal="true" aria-label="Settings">
      <div class="sheet-top">
        <span>Settings</span><span class="grow"></span>
        <button class="ibtn" data-close aria-label="Close">
          <svg width="16" height="16" viewBox="0 0 16 16" fill="none"><path d="M4.5 4.5l7 7M11.5 4.5l-7 7" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/></svg>
        </button>
      </div>
      <div class="sheet-body">
        <div class="sheet-nav"></div>
        <div class="sheet-page"></div>
      </div>
      <div class="sheet-foot">
        <span class="grow"></span>
        <button class="pill" data-close>Cancel</button>
        <button class="primary" data-save>Save</button>
      </div>
    </div>`;
  document.body.append(back);
  sheetEl = back;

  const PAGES = [
    ["general", "General"],
    ["appearance", "Appearance"],
    ["toolbars", "Toolbars"],
    ["reading", "Reading"],
    ["labels", "Labels"],
    ["filters", "Filters"],
    ["cleanup", "Storage &amp; cleanup"],
    ["shortcuts", "Shortcuts"],
    ["updates", "Updates"],
    ["about", "About"],
  ];
  const nav = back.querySelector(".sheet-nav");
  nav.innerHTML = PAGES.map(([k, n]) =>
    `<button data-page="${k}" aria-current="${k === page}">${n}</button>`).join("");

  let renderTicket = 0;
  const render = async (k) => {
    page = k;
    keyCapture = null;
    const ticket = ++renderTicket;
    nav.querySelectorAll("button").forEach((b) =>
      b.setAttribute("aria-current", String(b.dataset.page === k)));
    const host = back.querySelector(".sheet-page");
    const html = await settingsPage(k, pending);
    // A slower page finishing after a quicker click must not replace it.
    if (ticket !== renderTicket) return;
    host.innerHTML = html;
    paint(host);
    host.scrollTop = 0;
    wireSheet(host, pending, render);
  };

  nav.onclick = (e) => { const b = e.target.closest("[data-page]"); if (b) render(b.dataset.page); };
  back.querySelectorAll("[data-close]").forEach((b) => (b.onclick = closeSheet));
  back.onclick = (e) => { if (e.target === back) closeSheet(); };
  back.querySelector("[data-save]").onclick = async () => {
    try {
      const toSave = { ...pending };
      delete toSave._autostart;
      await invoke("set_settings", { values: toSave });
      // The close handler is synchronous and cannot read the database, so it
      // gets told directly.
      await invoke("set_close_to_tray", { on: toSave["startup.close_to_tray"] === "1" })
        .catch(() => {});
      applyTrayBehaviour(toSave);
      closeSheet();
      toast("Settings saved");
      await loadTree(); await loadList();
      refreshStatus().catch(() => {}); // also redraws the tray's count
    } catch (e) { toast(String(e)); }
  };
  // Bound to this sheet. Checking only "is a sheet open" left the listener of
  // every earlier sheet running, so one Esc closed a dialog and then, through
  // the next listener, Settings as well.
  const thisSheet = sheetEl;
  document.addEventListener("keydown", function esc(e) {
    if (sheetEl !== thisSheet) { document.removeEventListener("keydown", esc, true); return; }
    if (e.key !== "Escape") return;
    e.stopPropagation();
    // A dialog opened from Settings closes first, on its own. Esc used to
    // close Settings underneath it too, and orphaned the filter editor.
    const top = [...document.querySelectorAll(".modal-back")].pop();
    if (top) top.click();
    else closeSheet();
  }, true);

  await render(page);
}

async function settingsPage(k, v) {
  const theme = document.documentElement.dataset.theme || "system";
  const density = document.documentElement.dataset.density || "relaxed";
  const on = (key) => v[key] === "1";

  if (k === "general") return `
    <div class="grp">UPDATING</div>
    <div class="fld">
      <label for="s-iv">Check feeds every</label>
      <input id="s-iv" type="number" min="1" max="1440" value="${esc(v["update.interval_minutes"])}" data-num="update.interval_minutes">
      <span class="note">minutes</span>
    </div>
    <div class="fld">
      <label>Update on start</label>
      ${sw("update.on_startup", on("update.on_startup"))}
    </div>

    <div class="grp">STARTUP</div>
    <div class="fld">
      <label>Start SnapRSS when you log in</label>
      <span class="sw" role="switch" tabindex="0" aria-checked="${v._autostart === "1"}" data-sw="_autostart"><i></i></span>
    </div>
    <div class="fld">
      <label>Start minimised</label>
      ${sw("startup.minimized", on("startup.minimized"))}
    </div>
    <div class="fld">
      <label>Closing the window hides it to the tray</label>
      ${sw("startup.close_to_tray", on("startup.close_to_tray"))}
    </div>
    <div class="fld">
      <label>Minimising hides it to the tray</label>
      ${sw("startup.minimize_to_tray", on("startup.minimize_to_tray"))}
    </div>
    <div class="fld">
      <label>Show the unread count on the tray icon</label>
      ${sw("tray.show_unread", on("tray.show_unread"))}
    </div>

    <div class="grp">SUBSCRIPTIONS</div>
    <div class="fld">
      <label>Import OPML or a QuiteRSS database</label>
      <button class="pill" data-act="import">Import…</button>
    </div>
    <div class="fld">
      <label>Export OPML</label>
      <button class="pill" data-act="export">Export…</button>
    </div>`;

  if (k === "appearance") return `
    <div class="grp">THEME</div>
    <div class="themes">
      ${Object.entries(THEME_NAMES).map(([key, n]) => themeCard(key, n, theme === key)).join("")}
    </div>

    <div class="grp">DENSITY</div>
    <div class="fld">
      <label>Article list</label>
      <div class="seg" data-density-pick>
        <button data-d="relaxed" aria-pressed="${density === "relaxed"}">Relaxed</button>
        <button data-d="compact" aria-pressed="${density === "compact"}">Compact</button>
      </div>
    </div>
    <div class="fld">
      <label>Categories panel</label>
      ${sw("_cats", $("#cats-head").getAttribute("aria-expanded") === "true")}
    </div>

    <div class="grp">READING PANE</div>
    <div class="fld">
      <label>Article text size</label>
      <span class="grow1"></span>
      <button class="mini" data-textact="smaller" title="Smaller (${esc(keyHint("textSmaller") || "")})">A&#8722;</button>
      <span class="textsize" data-textsize>${Math.round(textScale() * 100)}%</span>
      <button class="mini" data-textact="bigger" title="Larger (${esc(keyHint("textBigger") || "")})">A+</button>
      <button class="pill" data-textact="reset">Reset</button>
    </div>
    <div class="fld"><span class="note">Stored on this computer. Ctrl + mouse wheel over an article works too.</span></div>`;

  if (k === "toolbars") {
    const cfg = toolbarConfig();
    const STYLES = [["icon", "Icons"], ["icontext", "Icons and text"], ["text", "Text only"]];
    return Object.entries(TOOLBARS).map(([key, spec]) => {
      const c = cfg[key];
      const chosen = c.items;
      const rest = ALL_COMMANDS.filter((x) => !chosen.includes(x));
      const locked = (item) => item === "appmenu" && key === "main";
      return `
      <div class="grp">${spec.name.toUpperCase()} TOOLBAR</div>
      <div class="fld">
        <label>Show this toolbar</label>
        <span class="sw" role="switch" tabindex="0" aria-checked="${c.show}" data-tbshow="${key}"><i></i></span>
      </div>
      <div class="fld">
        <label>Buttons</label>
        <select data-tbstyle="${key}">
          ${STYLES.map(([v, n]) =>
            `<option value="${v}"${c.style === v ? " selected" : ""}>${n}</option>`).join("")}
        </select>
      </div>
      <div class="tb-list">
        ${chosen.map((item, i) => `
          <div class="tb-item">
            <span class="en">${item.startsWith("sep")
              ? '<span class="dim">— separator —</span>'
              : esc(COMMANDS[item]?.name || item)}</span>
            <button class="mini" data-tbmove="${key}:${i}:-1" title="Move up" ${i === 0 ? "disabled" : ""}>&#9650;</button>
            <button class="mini" data-tbmove="${key}:${i}:1" title="Move down" ${i === chosen.length - 1 ? "disabled" : ""}>&#9660;</button>
            <button class="mini danger" data-tbdrop="${key}:${i}" title="${locked(item) ? "The menu has to stay somewhere" : "Remove"}" ${locked(item) ? "disabled" : ""}>&#215;</button>
          </div>`).join("")}
      </div>
      <div class="fld">
        <label>Add</label>
        <select data-tbadd="${key}">
          <option value="">Choose a button…</option>
          ${rest.map((x) => `<option value="${x}">${esc(COMMANDS[x].name)}</option>`).join("")}
          <option value="sep">— separator —</option>
        </select>
        <button class="pill" data-tbreset="${key}">Reset</button>
      </div>`;
    }).join("") + `
      <div class="fld"><span class="note">Stored on this computer. Any button can go on any toolbar.</span></div>`;
  }

  if (k === "labels") {
    const rows = await invoke("labels").catch(() => []);
    return `
    <div class="grp">LABELS</div>

    ${rows.length ? rows.map((l, i) => `
      <div class="erow">
        <span class="swatch" data-bg="${esc(l.color_bg || "var(--ink-4)")}"></span>
        <span class="en">${esc(l.name)}</span>
        <span class="sub2">${l.count} article${l.count === 1 ? "" : "s"}</span>
        <button class="mini" data-lmove="${l.id}:-1" title="Move up" ${i === 0 ? "disabled" : ""}>&#9650;</button>
        <button class="mini" data-lmove="${l.id}:1" title="Move down" ${i === rows.length - 1 ? "disabled" : ""}>&#9660;</button>
        <button class="mini" data-ledit="${l.id}" title="Edit">&#9998;</button>
        <button class="mini danger" data-ldel="${l.id}" title="Delete">&#215;</button>
      </div>`).join("")
      : '<div class="note pad">No labels yet.</div>'}
    <div class="fld"><button class="pill" data-lnew>New label…</button></div>`;
  }

  if (k === "filters") {
    const rows = await invoke("filters").catch(() => []);
    const MODE = ["every article", "all conditions", "any condition"];
    return `
    <div class="grp">FILTERS</div>
    <div class="fld"><span class="note">Applied to new articles in this order.</span></div>
    ${rows.length ? rows.map((f, i) => `
      <div class="erow${f.enabled ? "" : " off"}">
        <span class="sw" role="switch" tabindex="0" aria-checked="${f.enabled}" data-fen="${f.id}"><i></i></span>
        <span class="en">${esc(f.name)}${f.broken ? ' <span class="warn">bad regular expression</span>' : ""}</span>
        <span class="sub2">${MODE[f.mode] || "?"} &middot; ${f.conditions.length} condition${f.conditions.length === 1 ? "" : "s"}</span>
        <button class="mini" data-fmove="${f.id}:-1" title="Move up" ${i === 0 ? "disabled" : ""}>&#9650;</button>
        <button class="mini" data-fmove="${f.id}:1" title="Move down" ${i === rows.length - 1 ? "disabled" : ""}>&#9660;</button>
        <button class="mini" data-fedit="${f.id}" title="Edit">&#9998;</button>
        <button class="mini danger" data-fdel="${f.id}" title="Delete">&#215;</button>
      </div>`).join("")
      : '<div class="note pad">No filters yet.</div>'}
    <div class="fld">
      <button class="pill" data-fnew>New filter…</button>
      <button class="pill" data-fapply${rows.length ? "" : " disabled"}>Run on existing…</button>
    </div>`;
  }

  if (k === "reading") return `
    <div class="grp">THE READING PANE</div>
    <div class="fld">
      <label>New feeds show the feed summary only</label>
      ${sw("reading.default_description_only", on("reading.default_description_only"))}
    </div>
    <div class="fld">
      <label>Mark read on open</label>
      ${sw("reading.mark_read_on_open", on("reading.mark_read_on_open"))}
    </div>
    <div class="fld">
      <label>Enter opens in browser</label>
      ${sw("reading.enter_opens_browser", on("reading.enter_opens_browser"))}
    </div>
    <div class="fld">
      <span class="note">Each feed can override these in its properties.</span>
    </div>`;

  if (k === "cleanup") {
    const st = await invoke("db_stats").catch(() => null);
    return `
    <div class="grp">RETENTION DEFAULTS</div>
    <div class="fld"><span class="note">Used by feeds without their own rules. Runs hourly, and moves articles to Deleted.</span></div>
    <div class="fld">
      <label>Delete once read</label>
      ${sw("cleanup.delete_read", on("cleanup.delete_read"))}
    </div>
    <div class="fld">
      <label for="s-age">Delete older than</label>
      <input id="s-age" type="number" min="0" max="3650" placeholder="never" value="${esc(v["cleanup.max_age_days"])}" data-num="cleanup.max_age_days">
      <span class="note">days &mdash; blank for never</span>
    </div>
    <div class="fld">
      <label for="s-keep">Keep at most</label>
      <input id="s-keep" type="number" min="0" max="100000" placeholder="all" value="${esc(v["cleanup.max_to_keep"])}" data-num="cleanup.max_to_keep">
      <span class="note">per feed &mdash; blank for all</span>
    </div>

    <div class="grp">NEVER DELETE</div>
    <div class="fld sub"><label>Unread articles</label>${sw("cleanup.never_delete_unread", on("cleanup.never_delete_unread"))}</div>
    <div class="fld sub"><label>Starred articles</label>${sw("cleanup.never_delete_starred", on("cleanup.never_delete_starred"))}</div>
    <div class="fld sub"><label>Labelled articles</label>${sw("cleanup.never_delete_labeled", on("cleanup.never_delete_labeled"))}</div>

    <div class="grp">DATABASE</div>
    ${st ? `
    <div class="stat"><span>File size</span><span class="v">${fmtBytes(st.bytes)}</span></div>
    <div class="stat"><span>Feeds</span><span class="v">${st.feeds}</span></div>
    <div class="stat"><span>Articles</span><span class="v">${st.articles.toLocaleString()} (${st.unread.toLocaleString()} unread, ${st.starred} starred)</span></div>
    <div class="stat"><span>In the Deleted folder</span><span class="v">${st.deleted.toLocaleString()}</span></div>
    <div class="stat"><span>Cached article text</span><span class="v">${st.with_cached_article.toLocaleString()} articles</span></div>
    <div class="stat"><span>Oldest article</span><span class="v">${st.oldest ? new Date(st.oldest).toLocaleDateString() : "&mdash;"}</span></div>` : ""}

    <div class="danger-zone">
      <div class="fld">
        <label>Clean up now</label>
        <button class="pill" data-act="cleanup">Clean up</button>
      </div>
      <div class="fld">
        <label>Drop cached article text</label>
        <button class="pill" data-act="clearcache">Clear cache</button>
      </div>
      <div class="fld">
        <label>Empty Deleted <span class="note">&mdash; permanent</span></label>
        <button class="pill" data-act="purge">Empty</button>
      </div>
      <div class="fld">
        <label>Compact the database <span class="note">&mdash; slow on a large file</span></label>
        <button class="pill" data-act="vacuum">Compact</button>
      </div>
      <div class="fld">
        <label>Back up</label>
        <button class="pill" data-act="backup">Back up…</button>
      </div>
    </div>`;
  }

  if (k === "shortcuts") {
    const map = keymap();
    const rows = (ids) => ids.map((id) => {
      const [, label, def] = KEY_ACTIONS.find((a) => a[0] === id);
      const cur = map[id];
      return `<span>${esc(label)}</span>
        <button class="keybtn" data-rebind="${id}" title="Change">${cur ? `<kbd>${esc(keyLabel(cur))}</kbd>` : '<span class="faint">none</span>'}</button>
        ${cur !== def
          ? `<button class="mini" data-keyreset="${id}" title="Back to ${esc(keyLabel(def))}">&#8634;</button>`
          : "<span></span>"}`;
    }).join("");
    return KEY_GROUPS.map(([g, ids]) => `
      <div class="grp">${g}</div>
      <div class="keyedit">${rows(ids)}</div>`).join("") + `
      <div class="grp">FIXED</div>
      <div class="keys">
        <kbd>Enter</kbd><span>Open in browser</span>
        <kbd>Esc</kbd><span>Clear the selection</span>
        <kbd>Ctrl click</kbd><span>Add or remove one</span>
        <kbd>Shift click</kbd><span>Select a range</span>
        <kbd>&#8592; &#8594;</kbd><span>Fold and unfold a folder</span>
      </div>
      <div class="fld">
        <span class="note grow1">Click a shortcut, then press the new key. Backspace removes it, Esc cancels.
          Stored on this computer.</span>
        <button class="pill" data-keyresetall>Reset all</button>
      </div>`;
  }

  if (k === "updates") return `
    <div class="grp">UPDATES</div>
    <div class="fld">
      <label for="s-repo">Release repository</label>
      <input id="s-repo" type="text" class="grow1" spellcheck="false" autocomplete="off"
             placeholder="masterrite/SnapRSS" value="${esc(v["updates.repo"])}" data-num="updates.repo">
    </div>
    <div class="fld"><span class="note">The GitHub repository the release workflow publishes to, as owner/name, or the full URL of a latest.json. Left empty, SnapRSS uses masterrite/SnapRSS.</span></div>
    <div class="fld">
      <label>Check for new versions daily</label>
      ${sw("updates.auto", on("updates.auto"))}
    </div>
    <div class="fld">
      <label>This is version ${esc(APP_VERSION)}</label>
      <button class="pill" data-act="checkupdate">Check now</button>
    </div>
    <div class="fld"><span class="note" data-updresult></span></div>`;

  return `
    <div class="aboutbox">
      <img class="applogo" src="logo.png" alt="" width="56" height="56">
      <div class="appname">SnapRSS</div>
      <div class="note mt5" data-version>Version ${esc(APP_VERSION)}</div>
    </div>
    <div class="note about">Apache-2.0.</div>`;
}

/// Wire the controls on whichever page is showing. Called on every render
/// because the page is rebuilt from scratch each time.
function wireSheet(host, pending, render) {
  wireToolbarPage(host, render);
  wireLabelPage(host, render);
  wireFilterPage(host, render);

  host.querySelectorAll("[data-sw]").forEach((el) => {
    const toggle = () => {
      const now = el.getAttribute("aria-checked") !== "true";
      el.setAttribute("aria-checked", String(now));
      const key = el.dataset.sw;
      // Keys starting with _ are UI or OS state, not database settings.
      if (key === "_cats") {
        $("#cats-head").click();
      } else if (key === "_autostart") {
        invoke("set_autostart", { on: now })
          .then((actual) => {
            el.setAttribute("aria-checked", String(actual));
            // Kept, or the switch showed the old state after changing page.
            pending._autostart = actual ? "1" : "0";
            if (actual !== now) toast("The system refused to change the startup entry");
          })
          .catch((e) => { el.setAttribute("aria-checked", String(!now)); toast(String(e)); });
      } else {
        pending[key] = now ? "1" : "0";
      }
    };
    el.onclick = toggle;
    el.onkeydown = (e) => {
      if (e.key === " " || e.key === "Enter") { e.preventDefault(); toggle(); }
    };
  });

  host.querySelectorAll("[data-num]").forEach((el) => {
    el.oninput = () => { pending[el.dataset.num] = el.value.trim(); };
  });

  host.querySelectorAll("[data-theme-pick]").forEach((el) => {
    el.onclick = () => {
      applyTheme(el.dataset.themePick);
      host.querySelectorAll("[data-theme-pick]").forEach((o) =>
        o.setAttribute("aria-checked", String(o === el)));
    };
  });

  host.querySelector("[data-density-pick]") &&
    (host.querySelector("[data-density-pick]").onclick = (e) => {
      const b = e.target.closest("button"); if (!b) return;
      setDensity(b.dataset.d);
      host.querySelectorAll("[data-density-pick] button").forEach((o) =>
        o.setAttribute("aria-pressed", String(o === b)));
    });

  host.querySelectorAll("[data-rebind]").forEach((b) => {
    b.onclick = () => {
      const id = b.dataset.rebind;
      host.querySelectorAll("[data-rebind]").forEach((o) => o.removeAttribute("aria-pressed"));
      b.setAttribute("aria-pressed", "true");
      b.innerHTML = '<span class="faint">Press a key…</span>';
      keyCapture = (e) => {
        // Settings went away some other way: stop listening, let the key through.
        if (!b.isConnected) { keyCapture = null; return; }
        e.preventDefault();
        e.stopImmediatePropagation();
        if (e.key === "Escape") { keyCapture = null; render("shortcuts"); return; }
        const map = keymap();
        if (e.key === "Backspace" && !e.ctrlKey && !e.altKey && !e.shiftKey) {
          keyCapture = null;
          map[id] = "";
          saveKeymap(map);
          render("shortcuts");
          return;
        }
        const combo = comboOf(e);
        if (!combo) return; // a modifier on its own: keep waiting
        if (["Enter", "Tab", "Shift+Tab"].includes(combo)) {
          toast(`${combo} is kept for the keyboard's usual job`);
          return;
        }
        keyCapture = null;
        // One key, one job: whatever had it gives it up.
        const other = KEY_ACTIONS.find(([o]) => o !== id && map[o] === combo);
        if (other) {
          map[other[0]] = "";
          toast(`${keyLabel(combo)} moved from “${other[1]}”`);
        }
        map[id] = combo;
        saveKeymap(map);
        render("shortcuts");
      };
    };
  });
  host.querySelectorAll("[data-keyreset]").forEach((b) => {
    b.onclick = () => {
      const id = b.dataset.keyreset;
      const map = keymap();
      const def = KEY_ACTIONS.find((a) => a[0] === id)[2];
      const other = KEY_ACTIONS.find(([o]) => o !== id && map[o] === def);
      if (other) map[other[0]] = "";
      map[id] = def;
      saveKeymap(map);
      render("shortcuts");
    };
  });
  host.querySelector("[data-keyresetall]") &&
    (host.querySelector("[data-keyresetall]").onclick = () => {
      try { localStorage.removeItem("keymap"); } catch {}
      refreshKeyTips();
      render("shortcuts");
    });

  host.querySelectorAll("[data-textact]").forEach((b) => {
    b.onclick = () => ({ bigger: textBigger, smaller: textSmaller, reset: textReset })[b.dataset.textact]();
  });

  host.querySelectorAll("[data-act]").forEach((b) => {
    b.onclick = async () => {
      const act = b.dataset.act;
      b.disabled = true;
      try {
        if (act === "import") { closeSheet(); return $("#btn-import").click(); }
        if (act === "export") { closeSheet(); return $("#btn-export").click(); }
        if (act === "backup") return await backupDatabase();
        if (act === "cleanup") {
          // Cleaning up uses saved rules. Unsaved changes to the rules are
          // saved first, when asked, and nothing else is: saving every
          // pending change here made Cancel meaningless, and left the running
          // app out of step with what it had just written.
          const saved = await invoke("get_settings").catch(() => ({}));
          const changed = Object.keys(pending).filter((k) =>
            k.startsWith("cleanup.") && (saved[k] ?? SETTING_DEFAULTS[k]) !== pending[k]);
          if (changed.length) {
            const ok = await ask({
              title: "Save the new cleanup rules and clean up with them?",
              placeholder: null, confirmLabel: "Save and clean up",
            });
            if (!ok) return;
            await invoke("set_settings", {
              values: Object.fromEntries(changed.map((k) => [k, pending[k]])),
            });
          }
          await runCleanupNow();
          return render("cleanup");
        }
        if (act === "purge") { await emptyTrash(); return render("cleanup"); }
        if (act === "checkupdate") {
          // The check reads the saved repository, so these two are saved
          // first; nothing else pending is.
          await invoke("set_settings", { values: {
            "updates.repo": pending["updates.repo"] ?? "",
            "updates.auto": pending["updates.auto"] ?? "1",
          } });
          const out = host.querySelector("[data-updresult]");
          out.textContent = "Checking…";
          try {
            const info = await invoke("check_update");
            out.textContent = info ? `Version ${info.version} is available.` : "This is the newest version.";
            if (info) showUpdateBar(info);
          } catch (e) {
            out.textContent = String(e);
          }
          return;
        }
        if (act === "clearcache") {
          const n = await invoke("clear_article_cache");
          toast(n ? `Cleared ${n} cached article${n === 1 ? "" : "s"}` : "Nothing cached");
          return render("cleanup");
        }
        if (act === "vacuum") {
          toast("Compacting…");
          const freed = await invoke("vacuum_db");
          toast(freed > 0 ? `Reclaimed ${fmtBytes(freed)}` : "Nothing to reclaim");
          return render("cleanup");
        }
      } catch (e) {
        toast(String(e));
      } finally {
        b.disabled = false;
      }
    };
  });
}

/// The toolbar page writes straight through rather than waiting for Save, the
/// same as theme and density, so the bar under the cursor changes as you edit.
function wireToolbarPage(host, render) {
  const edit = (fn) => { const c = toolbarConfig(); fn(c); saveToolbars(c); render("toolbars"); };

  host.querySelectorAll("[data-tbshow]").forEach((el) => {
    const toggle = () => edit((c) => { c[el.dataset.tbshow].show = el.getAttribute("aria-checked") !== "true"; });
    el.onclick = toggle;
    el.onkeydown = (e) => { if (e.key === " " || e.key === "Enter") { e.preventDefault(); toggle(); } };
  });

  host.querySelectorAll("[data-tbstyle]").forEach((el) => {
    el.onchange = () => edit((c) => { c[el.dataset.tbstyle].style = el.value; });
  });

  host.querySelectorAll("[data-tbmove]").forEach((b) => {
    const [key, i, d] = b.dataset.tbmove.split(":");
    b.onclick = () => edit((c) => {
      const a = c[key].items, from = +i, to = from + +d;
      if (to < 0 || to >= a.length) return;
      [a[from], a[to]] = [a[to], a[from]];
    });
  });

  host.querySelectorAll("[data-tbdrop]").forEach((b) => {
    const [key, i] = b.dataset.tbdrop.split(":");
    b.onclick = () => edit((c) => { c[key].items.splice(+i, 1); });
  });

  host.querySelectorAll("[data-tbadd]").forEach((el) => {
    el.onchange = () => {
      if (!el.value) return;
      const key = el.dataset.tbadd;
      edit((c) => {
        // Separators are the one thing that can repeat, so each gets a
        // distinct key: the config is de-duplicated on load.
        let item = el.value;
        if (item === "sep") {
          // The first free name. Counting existing ones reused a name still
          // in use after one was removed, and de-duplication then dropped it.
          let n = 1;
          while (c[key].items.includes("sep" + n)) n++;
          item = "sep" + n;
        }
        c[key].items.push(item);
      });
    };
  });

  host.querySelectorAll("[data-tbreset]").forEach((b) => {
    b.onclick = () => edit((c) => { c[b.dataset.tbreset] = structuredClone(TOOLBAR_DEFAULTS[b.dataset.tbreset]); });
  });
}

// ------------------------------------------------------------ label editor

const LABEL_COLOURS = ["#c2571f", "#b33a35", "#9c3d61", "#5e4390", "#2f6cb5",
                       "#2d7d6e", "#3f6b31", "#8a6d1f", "#4c525a"];

/// A small dialog of its own rather than a page, because a label is three
/// fields and a colour and does not deserve a whole screen.
function editLabel(existing) {
  if (modalOpen()) return Promise.resolve(null);
  return new Promise((resolve) => {
    let colour = existing?.color_bg || LABEL_COLOURS[0];
    const back = document.createElement("div");
    back.className = "modal-back";
    back.innerHTML = `
      <div class="modal" role="dialog" aria-modal="true" aria-label="Label">
        <div class="modal-title">${existing ? "Edit label" : "New label"}</div>
        <input class="modal-input" data-name placeholder="Label name"
               value="${esc(existing?.name || "")}" spellcheck="false" autocomplete="off">
        <div class="colors">
          ${LABEL_COLOURS.map((c) =>
            `<button data-c="${c}" data-bg="${c}" aria-pressed="${c === colour}" aria-label="${c}"></button>`).join("")}
        </div>
        <div class="modal-row">
          <button class="pill" data-cancel>Cancel</button>
          <button class="primary" data-ok>${existing ? "Save" : "Create"}</button>
        </div>
      </div>`;
    document.body.append(back);

    paint(back);
    const input = back.querySelector("[data-name]");
    input.focus();
    input.select();

    back.querySelectorAll("[data-c]").forEach((b) => {
      b.onclick = () => {
        colour = b.dataset.c;
        back.querySelectorAll("[data-c]").forEach((o) =>
          o.setAttribute("aria-pressed", String(o === b)));
      };
    });

    const done = (v) => { back.remove(); resolve(v); };
    back.querySelector("[data-cancel]").onclick = () => done(null);
    back.querySelector("[data-ok]").onclick = () => {
      const name = input.value.trim();
      if (!name) return input.focus();
      // Keep an imported label's own text colour.
      done({ id: existing?.id ?? null, name, colorBg: colour, colorText: existing?.color_text || "#ffffff" });
    };
    back.onclick = (e) => { if (e.target === back) done(null); };
    input.onkeydown = (e) => {
      if (e.key === "Enter") back.querySelector("[data-ok]").click();
      if (e.key === "Escape") { e.stopPropagation(); done(null); }
    };
  });
}

/// Edit one label: its name and colour. Used by the Labels page, the sidebar's
/// context menu and a double-click on the label in the sidebar.
async function editLabelById(id) {
  const cur = (await invoke("labels").catch(() => [])).find((l) => l.id === id);
  if (!cur) return false;
  const d = await editLabel(cur);
  if (!d) return false;
  try {
    await invoke("save_label", { draft: { id, name: d.name, colorBg: d.colorBg, colorText: d.colorText } });
  } catch (e) { toast(String(e)); return false; }
  await loadTree();
  await loadList();
  if (state.scope === "label:" + id) {
    state.scopeName = d.name;
    $("#scopename").textContent = d.name;
  }
  return true;
}

/// Delete one label after asking. The articles keep everything else.
async function deleteLabelById(id) {
  const cur = state.labels.find((l) => l.id === id);
  const ok = await ask({
    title: cur ? `Delete the label "${cur.name}"?` : "Delete this label?",
    placeholder: null, confirmLabel: "Delete", danger: true,
  });
  if (!ok) return false;
  await invoke("delete_label", { id });
  // Viewing the label that just went away leaves nothing to show.
  if (state.scope === "label:" + id) await clearScope();
  await loadTree();
  await loadList();
  return true;
}

function wireLabelPage(host, render) {
  const after = async () => { await loadTree(); await loadList(); render("labels"); };

  host.querySelector("[data-lnew]") &&
    (host.querySelector("[data-lnew]").onclick = async () => {
      const d = await editLabel(null);
      if (!d) return;
      try {
        await invoke("save_label", { draft: { id: null, name: d.name, colorBg: d.colorBg, colorText: d.colorText } });
        await after();
      } catch (e) { toast(String(e)); }
    });

  host.querySelectorAll("[data-ledit]").forEach((b) => {
    b.onclick = async () => {
      if (await editLabelById(Number(b.dataset.ledit))) render("labels");
    };
  });

  host.querySelectorAll("[data-ldel]").forEach((b) => {
    b.onclick = async () => {
      if (await deleteLabelById(Number(b.dataset.ldel))) render("labels");
    };
  });

  host.querySelectorAll("[data-lmove]").forEach((b) => {
    const [id, d] = b.dataset.lmove.split(":");
    b.onclick = async () => {
      await invoke("reorder_label", { id: Number(id), delta: Number(d) });
      await after();
    };
  });
}

// ----------------------------------------------------------- filter editor

let VOCAB = null;

async function vocabulary() {
  if (!VOCAB) VOCAB = await invoke("filter_vocabulary");
  return VOCAB;
}

const OP_NAMES = {
  contains: "contains", not_contains: "doesn't contain", is: "is", is_not: "isn't",
  begins_with: "begins with", ends_with: "ends with", regex: "matches regex",
};
const FIELD_NAMES = {
  title: "Title", description: "Body", author: "Author", category: "Category",
  status: "Status", link: "Link", news: "Title or body",
};
const ACTION_NAMES = {
  mark_read: "Mark as read", add_star: "Add a star", delete: "Delete",
  add_label: "Add a label", play_sound: "Play a sound", notify: "Show a notification",
};

/// The editor for one filter. Conditions and actions are rows you add and
/// remove; the operator list is re-read from the backend's vocabulary whenever
/// the field changes, so the two can never disagree about what is legal.
async function editFilter(existing, labels) {
  if (modalOpen()) return null;
  const v = await vocabulary();
  if (modalOpen()) return null;
  const feeds = [];
  (function walk(ns) { for (const n of ns) { if (!n.is_folder) feeds.push(n); walk(n.children || []); } })(state.tree);

  const model = {
    id: existing?.id ?? null,
    name: existing?.name || "",
    mode: existing?.mode ?? 1,
    enabled: existing?.enabled ?? true,
    feeds: existing?.feeds ?? null,
    conditions: existing?.conditions?.length
      ? existing.conditions.map((c) => ({ ...c }))
      : [{ field: "title", op: "contains", content: "" }],
    actions: existing?.actions?.length
      ? existing.actions.map((a) => ({ ...a }))
      : [{ action: "mark_read", params: null }],
  };

  return new Promise((resolve) => {
    const back = document.createElement("div");
    back.className = "modal-back";
    back.innerHTML = `<div class="modal wide" role="dialog" aria-modal="true" aria-label="Filter"></div>`;
    document.body.append(back);
    const box = back.querySelector(".modal");

    const opsFor = (field) => v.ops[field] || ["contains"];

    const draw = () => {
      box.innerHTML = `
        <div class="modal-title">${existing ? "Edit filter" : "New filter"}</div>
        <input class="modal-input" data-name placeholder="Filter name"
               value="${esc(model.name)}" spellcheck="false" autocomplete="off">

        <div class="grp">MATCH</div>
        <div class="fld">
          <select data-mode>
            <option value="1"${model.mode === 1 ? " selected" : ""}>All of these conditions</option>
            <option value="2"${model.mode === 2 ? " selected" : ""}>Any of these conditions</option>
            <option value="0"${model.mode === 0 ? " selected" : ""}>Every article</option>
          </select>
          <span class="note">in</span>
          <select data-scope>
            <option value=""${model.feeds === null ? " selected" : ""}>every feed</option>
            ${model.feeds !== null && !(model.feeds.length === 1 && feeds.some((f) => f.id === model.feeds[0]))
              ? `<option value="keep" selected>${model.feeds.length ? `${model.feeds.length} feeds` : "no feed"} (as set)</option>` : ""}
            ${feeds.map((f) => `<option value="${f.id}"${model.feeds?.length === 1 && model.feeds[0] === f.id ? " selected" : ""}>${esc(f.title)}</option>`).join("")}
          </select>
        </div>

        ${model.mode === 0 ? "" : model.conditions.map((c, i) => `
          <div class="crow">
            <select data-cf="${i}">
              ${v.fields.map((f) => `<option value="${f}"${c.field === f ? " selected" : ""}>${esc(FIELD_NAMES[f] || f)}</option>`).join("")}
            </select>
            <select data-co="${i}">
              ${opsFor(c.field).map((o) => `<option value="${o}"${c.op === o ? " selected" : ""}>${esc(OP_NAMES[o] || o)}</option>`).join("")}
            </select>
            ${c.field === "status"
              ? `<select data-cv="${i}">${v.statuses.map((s) =>
                  `<option value="${s}"${c.content === s ? " selected" : ""}>${s}</option>`).join("")}</select>`
              : `<input type="text" data-cv="${i}" value="${esc(c.content)}" placeholder="text" spellcheck="false">`}
            <button class="mini danger" data-cdel="${i}" title="Remove"
                    ${model.conditions.length === 1 ? "disabled" : ""}>&#215;</button>
          </div>`).join("")}
        ${model.mode === 0 ? "" : '<div class="fld"><button class="pill" data-cadd>Add a condition</button></div>'}

        <div class="grp">THEN</div>
        ${model.actions.map((a, i) => `
          <div class="crow">
            <select data-af="${i}">
              ${v.actions.map((x) => `<option value="${x}"${a.action === x ? " selected" : ""}>${esc(ACTION_NAMES[x] || x)}</option>`).join("")}
            </select>
            ${a.action === "add_label"
              ? `<select data-av="${i}">
                   ${labels.length
                     ? labels.map((l) => `<option value="${l.id}"${String(a.params) === String(l.id) ? " selected" : ""}>${esc(l.name)}</option>`).join("")
                     : '<option value="">no labels yet</option>'}
                 </select>`
              : a.action === "play_sound"
              ? `<input type="text" data-av="${i}" class="grow1" value="${esc(a.params || "")}"
                        placeholder="sound file" spellcheck="false">
                 <button class="mini" data-abrowse="${i}" title="Choose a sound file">…</button>
                 <button class="mini" data-aplay="${i}" title="Play it">&#9654;</button>`
              : ""}
            <span class="grow"></span>
            <button class="mini danger" data-adel="${i}" title="Remove"
                    ${model.actions.length === 1 ? "disabled" : ""}>&#215;</button>
          </div>`).join("")}
        <div class="fld"><button class="pill" data-aadd>Add an action</button></div>

        <div class="modal-row mt18">
          <button class="pill" data-cancel>Cancel</button>
          <button class="primary" data-ok>${existing ? "Save" : "Create"}</button>
        </div>`;

      box.querySelector("[data-name]").oninput = (e) => { model.name = e.target.value; };
      box.querySelector("[data-mode]").onchange = (e) => { model.mode = Number(e.target.value); draw(); };
      box.querySelector("[data-scope]").onchange = (e) => {
        // "keep" is the list the filter came with, which this select cannot
        // show one by one; choosing it again changes nothing.
        if (e.target.value !== "keep") model.feeds = e.target.value ? [Number(e.target.value)] : null;
      };

      box.querySelectorAll("[data-cf]").forEach((el) => {
        el.onchange = () => {
          const i = +el.dataset.cf;
          model.conditions[i].field = el.value;
          // The operator lists differ per field, so the previous choice may not
          // exist any more. Fall back to the first legal one.
          const ops = opsFor(el.value);
          if (!ops.includes(model.conditions[i].op)) model.conditions[i].op = ops[0];
          if (el.value === "status" && !v.statuses.includes(model.conditions[i].content))
            model.conditions[i].content = v.statuses[0];
          draw();
        };
      });
      box.querySelectorAll("[data-co]").forEach((el) => {
        el.onchange = () => { model.conditions[+el.dataset.co].op = el.value; };
      });
      box.querySelectorAll("[data-cv]").forEach((el) => {
        const set = () => { model.conditions[+el.dataset.cv].content = el.value; };
        el.oninput = set; el.onchange = set;
      });
      box.querySelectorAll("[data-cdel]").forEach((b) => {
        b.onclick = () => { model.conditions.splice(+b.dataset.cdel, 1); draw(); };
      });
      box.querySelector("[data-cadd]") && (box.querySelector("[data-cadd]").onclick = () => {
        model.conditions.push({ field: "title", op: "contains", content: "" }); draw();
      });

      box.querySelectorAll("[data-af]").forEach((el) => {
        el.onchange = () => {
          const i = +el.dataset.af;
          model.actions[i].action = el.value;
          model.actions[i].params = el.value === "add_label" ? (labels[0]?.id ?? null)
            : el.value === "play_sound" ? "" : null;
          draw();
        };
      });
      box.querySelectorAll("[data-av]").forEach((el) => {
        const set = () => { model.actions[+el.dataset.av].params = el.value; };
        el.onchange = set; el.oninput = set;
      });
      box.querySelectorAll("[data-abrowse]").forEach((b) => {
        b.onclick = async () => {
          let picked;
          try {
            picked = await dialog.open({
              title: "Sound to play", multiple: false, directory: false,
              filters: [{ name: "Sounds", extensions: ["wav", "mp3", "ogg", "oga", "opus", "m4a", "aac", "flac", "wma"] }],
            });
          } catch (e) { return toast(String(e)); }
          if (!picked) return;
          model.actions[+b.dataset.abrowse].params = String(picked.path ?? picked);
          draw();
        };
      });
      box.querySelectorAll("[data-aplay]").forEach((b) => {
        b.onclick = async () => {
          const path = model.actions[+b.dataset.aplay].params;
          if (!path) return toast("Choose a sound file first");
          try { await invoke("test_sound", { path }); } catch (e) { toast(String(e)); }
        };
      });
      box.querySelectorAll("[data-adel]").forEach((b) => {
        b.onclick = () => { model.actions.splice(+b.dataset.adel, 1); draw(); };
      });
      box.querySelector("[data-aadd]").onclick = () => {
        model.actions.push({ action: "mark_read", params: null }); draw();
      };

      box.querySelector("[data-cancel]").onclick = () => done(null);
      box.querySelector("[data-ok]").onclick = () => {
        if (!model.name.trim()) return box.querySelector("[data-name]").focus();
        if (model.actions.some((a) => a.action === "add_label" && !a.params))
          return toast("That filter adds a label, but there are no labels yet");
        if (model.actions.some((a) => a.action === "play_sound" && !String(a.params || "").trim()))
          return toast("Choose the sound file to play");
        done({
          id: model.id,
          name: model.name.trim(),
          mode: model.mode,
          enabled: model.enabled,
          feeds: model.feeds,
          conditions: model.mode === 0 ? [] : model.conditions,
          actions: model.actions.map((a) => ({
            action: a.action,
            params: a.params === null || a.params === undefined ? null : String(a.params),
          })),
        });
      };
    };

    const done = (val) => { back.remove(); resolve(val); };
    back.onclick = (e) => { if (e.target === back) done(null); };
    draw();
  });
}

function wireFilterPage(host, render) {
  const after = async () => { render("filters"); };

  host.querySelector("[data-fnew]") &&
    (host.querySelector("[data-fnew]").onclick = async () => {
      const labels = await invoke("labels").catch(() => []);
      const d = await editFilter(null, labels);
      if (!d) return;
      try { await invoke("save_filter", { draft: d }); await after(); }
      catch (e) { toast(String(e)); }
    });

  host.querySelectorAll("[data-fedit]").forEach((b) => {
    b.onclick = async () => {
      const id = Number(b.dataset.fedit);
      const [all, labels] = await Promise.all([
        invoke("filters").catch(() => []),
        invoke("labels").catch(() => []),
      ]);
      const cur = all.find((f) => f.id === id);
      if (!cur) return;
      const d = await editFilter(cur, labels);
      if (!d) return;
      try { await invoke("save_filter", { draft: d }); await after(); }
      catch (e) { toast(String(e)); }
    };
  });

  host.querySelectorAll("[data-fdel]").forEach((b) => {
    b.onclick = async () => {
      const ok = await ask({
        title: "Delete this filter?",
        placeholder: null, confirmLabel: "Delete", danger: true,
      });
      if (!ok) return;
      await invoke("delete_filter", { id: Number(b.dataset.fdel) });
      await after();
    };
  });

  host.querySelectorAll("[data-fmove]").forEach((b) => {
    const [id, d] = b.dataset.fmove.split(":");
    b.onclick = async () => {
      await invoke("reorder_filter", { id: Number(id), delta: Number(d) });
      await after();
    };
  });

  host.querySelectorAll("[data-fen]").forEach((el) => {
    const toggle = async () => {
      const on = el.getAttribute("aria-checked") !== "true";
      await invoke("set_filter_enabled", { id: Number(el.dataset.fen), on });
      await after();
    };
    el.onclick = toggle;
    el.onkeydown = (e) => { if (e.key === " " || e.key === "Enter") { e.preventDefault(); toggle(); } };
  });

  host.querySelector("[data-fapply]") &&
    (host.querySelector("[data-fapply]").onclick = async () => {
      // Filters normally only touch arriving articles. Running them over
      // everything already stored can mark thousands read or delete them, so
      // it is a deliberate button with a confirmation, not a side effect.
      const ok = await ask({
        title: "Run all enabled filters over existing articles?",
        placeholder: null, confirmLabel: "Run", danger: true,
      });
      if (!ok) return;
      try {
        const r = await invoke("apply_filters_now", { feedId: null, filterId: null });
        toast(r.matched
          ? `${r.matched} of ${r.considered} matched · ${r.marked_read} read, ${r.starred} starred, ${r.deleted} deleted`
          : `Nothing matched out of ${r.considered}`);
        await loadTree(); await loadList(); await refreshStatus();
      } catch (e) { toast(String(e)); }
    });
}

// --------------------------------------------------------- feed properties
//
// Per-feed overrides. Separate from the settings window on purpose: a
// retention rule that silently applies to every feed at once is a trap, so
// global defaults and one feed's exceptions are edited in different places.

async function openFeedSettings(id) {
  if (!id) return toast("Select a feed first");
  closeSheet();
  const ticket = ++sheetTicket;

  let f;
  try { f = await invoke("feed_settings", { id }); }
  catch (e) { return toast(String(e)); }
  if (ticket !== sheetTicket) return;
  closeSheet();

  const back = document.createElement("div");
  back.className = "sheet-back";
  back.innerHTML = `
    <div class="sheet" data-size="props"
         role="dialog" aria-modal="true" aria-label="Feed properties">
      <div class="sheet-top">
        <span>Feed properties</span><span class="grow"></span>
        <button class="ibtn" data-close aria-label="Close">
          <svg width="16" height="16" viewBox="0 0 16 16" fill="none"><path d="M4.5 4.5l7 7M11.5 4.5l-7 7" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/></svg>
        </button>
      </div>
      <div class="sheet-page">
        <div class="grp">FEED</div>
        <div class="fld">
          <label for="f-name" class="flabel">Name</label>
          <input id="f-name" type="text" class="grow1" value="${esc(f.title)}">
        </div>
        <div class="fld">
          <label class="flabel">Address</label>
          <span class="note grow1 mono11">${esc(f.xmlUrl || "")}</span>
        </div>
        <div class="fld">
          <label class="flabel">Status</label>
          <span class="note grow1">${f.status
            ? `<span class="warn">${esc(f.status)}</span>`
            : "Working"} &middot; ${f.articleCount} articles${
            f.updated ? " &middot; updated " + new Date(f.updated).toLocaleString() : ""}</span>
        </div>

        <div class="grp">SIGN-IN</div>
        <div class="fld"><span class="note">For feeds that ask for a user name and password. Shared by
          every feed on the same site.</span></div>
        <div class="fld">
          <label for="f-user" class="flabel">User name</label>
          <input id="f-user" type="text" class="grow1" autocomplete="off" spellcheck="false"
                 value="${esc(f.signInUser || "")}">
        </div>
        <div class="fld">
          <label for="f-pass" class="flabel">Password</label>
          <input id="f-pass" type="password" class="grow1" autocomplete="new-password"
                 placeholder="${f.hasPassword ? "Saved; type to change" : ""}">
        </div>

        <div class="grp">READING</div>
        <div class="fld">
          <label>Show the feed summary only</label>
          ${sw("descriptionOnly", f.descriptionOnly)}
        </div>
        <div class="fld"><label>Load images</label>${sw("loadImages", f.loadImages)}</div>
        <div class="fld"><label>Keep a copy offline</label>${sw("saveOffline", f.saveOffline)}</div>
        <div class="fld"><label>Right-to-left text</label>${sw("rtl", f.layoutDirection === 1)}</div>

        <div class="grp">UPDATING</div>
        <div class="fld"><label>Never update this feed</label>${sw("disableUpdate", f.disableUpdate)}</div>
        <div class="fld">
          <label>Use its own interval</label>
          ${sw("ivEnable", f.updateIntervalEnable)}
        </div>
        <div class="fld sub">
          <input id="f-iv" type="number" min="1" max="10000" value="${f.updateInterval ?? 15}">
          <select id="f-ivt">
            ${["seconds", "minutes", "hours", "days"].map((u) =>
              `<option value="${u}" ${(f.updateIntervalType || "minutes") === u ? "selected" : ""}>${u}</option>`).join("")}
          </select>
        </div>
        <div class="fld"><label>Delete duplicates</label>${sw("dupes", f.duplicateNewsMode)}</div>

        <div class="grp">RETENTION</div>
        <div class="fld"><span class="note">Off means use the Settings defaults.</span></div>
        <div class="fld"><label>Delete once read</label>${sw("deleteRead", f.deleteRead)}</div>
        <div class="fld">
          <label>Keep at most</label>${sw("keepEnable", f.maxToKeepEnable)}
          <input id="f-keep" type="number" min="1" max="100000" value="${f.maxToKeep ?? 200}">
          <span class="note">articles</span>
        </div>
        <div class="fld">
          <label>Delete older than</label>${sw("ageEnable", f.maxAgeEnable)}
          <input id="f-age" type="number" min="1" max="3650" value="${f.maxAgeDays ?? 90}">
          <span class="note">days</span>
        </div>
        <div class="fld sub"><label>Never delete unread</label>${sw("keepUnread", f.neverDeleteUnread)}</div>
        <div class="fld sub"><label>Never delete starred</label>${sw("keepStarred", f.neverDeleteStarred)}</div>
        <div class="fld sub"><label>Never delete labelled</label>${sw("keepLabeled", f.neverDeleteLabeled)}</div>
      </div>
      <div class="sheet-foot">
        <button class="pill" data-apply-folder>Apply to all feeds</button>
        <span class="grow"></span>
        <button class="pill" data-close>Cancel</button>
        <button class="primary" data-save>Save</button>
      </div>
    </div>`;
  document.body.append(back);
  sheetEl = back;

  const flags = {};
  back.querySelectorAll("[data-sw]").forEach((el) => {
    flags[el.dataset.sw] = el.getAttribute("aria-checked") === "true";
    const toggle = () => {
      flags[el.dataset.sw] = el.getAttribute("aria-checked") !== "true";
      el.setAttribute("aria-checked", String(flags[el.dataset.sw]));
    };
    el.onclick = toggle;
    el.onkeydown = (e) => {
      if (e.key === " " || e.key === "Enter") { e.preventDefault(); toggle(); }
    };
  });

  const collect = () => ({
    id: f.id,
    title: back.querySelector("#f-name").value.trim() || f.title,
    xmlUrl: f.xmlUrl,
    htmlUrl: f.htmlUrl,
    descriptionOnly: flags.descriptionOnly,
    loadImages: flags.loadImages,
    saveOffline: flags.saveOffline,
    showNotification: f.showNotification,
    disableUpdate: flags.disableUpdate,
    layoutDirection: flags.rtl ? 1 : 0,
    updateIntervalEnable: flags.ivEnable,
    updateInterval: Number(back.querySelector("#f-iv").value) || 15,
    updateIntervalType: back.querySelector("#f-ivt").value,
    duplicateNewsMode: flags.dupes,
    maxToKeepEnable: flags.keepEnable,
    maxToKeep: Number(back.querySelector("#f-keep").value) || null,
    maxAgeEnable: flags.ageEnable,
    maxAgeDays: Number(back.querySelector("#f-age").value) || null,
    deleteRead: flags.deleteRead,
    neverDeleteUnread: flags.keepUnread,
    neverDeleteStarred: flags.keepStarred,
    neverDeleteLabeled: flags.keepLabeled,
    signInUser: back.querySelector("#f-user").value.trim(),
    signInPassword: back.querySelector("#f-pass").value || null,
    hasPassword: f.hasPassword,
    status: f.status,
    updated: f.updated,
    articleCount: f.articleCount,
  });

  back.querySelectorAll("[data-close]").forEach((b) => (b.onclick = closeSheet));
  back.onclick = (e) => { if (e.target === back) closeSheet(); };

  back.querySelector("[data-save]").onclick = async () => {
    try {
      const saved = collect();
      await invoke("save_feed_settings", { s: saved });
      closeSheet();
      toast("Saved");
      await renamed(f.id, saved.title);
    } catch (e) { toast(String(e)); }
  };

  back.querySelector("[data-apply-folder]").onclick = async () => {
    const ok = await ask({
      title: "Apply these settings to all feeds?",
      placeholder: null, confirmLabel: "Apply to all",
    });
    if (!ok) return;
    try {
      await invoke("save_feed_settings", { s: collect() });
      const n = await invoke("apply_settings_to_folder", { fromId: f.id, folderId: null });
      closeSheet();
      toast(`Applied to ${n} other feed${n === 1 ? "" : "s"}`);
      await loadTree();
    } catch (e) { toast(String(e)); }
  };
}

// ------------------------------------------------------------- the four bars
//
// Main, Feeds, News and Reading, as QuiteRSS has them. Customisation moves and
// hides buttons that are already in the document; it never builds them, so the
// handlers bound once at startup survive every change. That is the whole trick
// — rebuilding the markup from a registry would silently drop every onclick.
//
// Like theme and density, this is a per-machine preference and lives in
// localStorage rather than the database.

/// Every command a toolbar can show, with the element that already exists in
/// the document for it. A command can appear on more than one bar: the node is
/// cloned once per bar at startup and the clone is wired to the same action,
/// which is what lets "Mark all read" sit on the Feeds bar as well as News.
const COMMANDS = {
  appmenu:    { name: "Menu",             home: "main" },
  update:     { name: "Update all",       home: "main" },
  addfeed:    { name: "Add feed",         home: "main" },
  addfolder:  { name: "Add folder",       home: "feeds" },
  refreshone: { name: "Update this feed", home: "feeds" },
  feedprops:  { name: "Feed properties",  home: "feeds" },
  removefeed: { name: "Remove feed",      home: "feeds" },
  star:       { name: "Star",             home: "news" },
  toggleread: { name: "Toggle read",      home: "news" },
  label:      { name: "Labels",           home: "news" },
  delete:     { name: "Delete",           home: "news" },
  markall:    { name: "Mark all read",    home: "news" },
  prev:       { name: "Previous",         home: "reading" },
  next:       { name: "Next",             home: "reading" },
  openext:    { name: "Open in browser",  home: "reading" },
  layout:     { name: "Layout switch",    home: "main" },
  density:    { name: "Density switch",   home: "main" },
  theme:      { name: "Theme",            home: "main" },
  import:     { name: "Import",           home: "main" },
  export:     { name: "Export",           home: "main" },
  settings:   { name: "Settings",         home: "main" },
};

const TOOLBARS = {
  main:    { name: "Main",    el: "#toolbar" },
  feeds:   { name: "Feeds",   el: "#feedsbar" },
  news:    { name: "News",    el: "#listbar" },
  reading: { name: "Reading", el: "#readbar" },
};

const ALL_COMMANDS = Object.keys(COMMANDS);

const TOOLBAR_DEFAULTS = {
  main:    { show: true, style: "icontext", items: ["appmenu", "update", "addfeed", "sep", "layout", "density", "theme", "sep2", "import", "export", "settings"] },
  feeds:   { show: true, style: "icon",     items: ["addfeed", "addfolder", "sep", "refreshone", "feedprops", "removefeed"] },
  news:    { show: true, style: "icon",     items: ["star", "toggleread", "label", "delete", "sep", "markall"] },
  reading: { show: true, style: "icon",     items: ["prev", "next", "sep", "star", "openext"] },
};

/// Clone the one real element for a command into every other bar, so any
/// command can be placed anywhere. The clone's click is forwarded to the
/// original, which keeps a single handler per command.
/// The toolbar button a forwarded click came from, while it is handled.
let toolbarAnchor = null;

function seedToolbarClones() {
  for (const [cmd, spec] of Object.entries(COMMANDS)) {
    const home = $(`${TOOLBARS[spec.home].el} [data-cmd="${cmd}"]`);
    if (!home) continue;
    home.dataset.home = "1";
    for (const [key, bar] of Object.entries(TOOLBARS)) {
      if (key === spec.home) continue;
      const host = $(bar.el);
      if (!host || host.querySelector(`[data-cmd="${cmd}"]`)) continue;
      const clone = home.cloneNode(true);
      clone.removeAttribute("id");
      delete clone.dataset.home;
      clone.hidden = true;
      // Segmented controls carry their own listeners; forward from the clone.
      clone.addEventListener("click", (e) => {
        // `button[...]` matters: <html> carries data-layout and data-density,
        // so an unqualified closest() walks all the way up to it and every
        // click looks like a segmented-control click.
        const seg = e.target.closest("button[data-layout], button[data-density]");
        if (seg) {
          const twin = home.querySelector(
            seg.dataset.layout ? `[data-layout="${seg.dataset.layout}"]`
                               : `[data-density="${seg.dataset.density}"]`);
          twin?.click();
        } else {
          // Menus open under the button that was clicked, not under the
          // hidden original (or the window's corner).
          toolbarAnchor = clone;
          try { home.click(); } finally { toolbarAnchor = null; }
        }
        e.stopPropagation();
      });
      host.insertBefore(clone, host.querySelector(".grow") || null);
    }
  }
}

function toolbarConfig() {
  let saved = {};
  try { saved = JSON.parse(localStorage.getItem("toolbars") || "{}"); } catch { saved = {}; }
  const out = {};
  for (const [k, def] of Object.entries(TOOLBAR_DEFAULTS)) {
    const s = saved[k] || {};
    const known = new Set(ALL_COMMANDS);
    // Drop anything the current build no longer has, and de-duplicate, so a
    // hand-edited or stale entry cannot produce two copies of one button.
    //
    // The fallback is a *copy* of the default. Handing out the module's own
    // array meant the first edit mutated the defaults in place, and Reset then
    // faithfully restored the corrupted version.
    // Separators are not commands and are allowed to repeat, so they are
    // kept by prefix. Filtering them out silently renumbered every item after
    // the first one.
    const items = Array.isArray(s.items)
      ? [...new Set(s.items.filter((x) => known.has(x) || /^sep/.test(x)))]
      : [...def.items];
    out[k] = {
      show: s.show === undefined ? def.show : !!s.show,
      style: ["icon", "icontext", "text"].includes(s.style) ? s.style : def.style,
      items,
    };
  }
  // The app menu is the only way back to Settings when every bar is hidden, so
  // it is never allowed to disappear. If it has been taken off the main bar,
  // or the main bar is hidden, it goes back on and the bar comes back.
  const onSomeBar = Object.values(out).some((c) => c.show && c.items.includes("appmenu"));
  if (!onSomeBar) {
    out.main.show = true;
    if (!out.main.items.includes("appmenu")) out.main.items.unshift("appmenu");
  }
  return out;
}

function saveToolbars(cfg) {
  localStorage.setItem("toolbars", JSON.stringify(cfg));
  // Re-read rather than applying what was passed, so the "the menu has to be
  // reachable" rule runs on every save and not only on the next start.
  const normalised = toolbarConfig();
  localStorage.setItem("toolbars", JSON.stringify(normalised));
  applyToolbars(normalised);
}

/// Reorder and hide. `appendChild` on an element already in the document moves
/// it, keeping its listeners, which is why nothing here re-creates a button.
function applyToolbars(cfg = toolbarConfig()) {
  for (const [key, spec] of Object.entries(TOOLBARS)) {
    const bar = $(spec.el);
    if (!bar) continue;
    const conf = cfg[key];

    bar.hidden = !conf.show;
    bar.dataset.style = conf.style;

    // Separators are the only things safe to destroy: they carry no state.
    bar.querySelectorAll(".sep").forEach((s) => s.remove());

    const wanted = new Set(conf.items);
    const byCmd = new Map();
    bar.querySelectorAll("[data-cmd]").forEach((el) => {
      byCmd.set(el.dataset.cmd, el);
      el.hidden = !wanted.has(el.dataset.cmd);
    });

    const grow = bar.querySelector(".grow");
    for (const item of conf.items) {
      if (item.startsWith("sep")) {
        const sep = document.createElement("div");
        sep.className = "sep";
        grow ? bar.insertBefore(sep, grow) : bar.append(sep);
        continue;
      }
      const el = byCmd.get(item);
      if (!el) continue;
      grow ? bar.insertBefore(el, grow) : bar.append(el);
    }
  }
}

// ---------------------------------------------------------------- resizing
//
// Widths and the categories height are CSS custom properties on :root, so a
// drag is one property write and the grid re-lays itself. Pointer capture
// keeps the drag alive when the pointer outruns the 4px grip.

const PANES = {
  "grip-sidebar": { prop: "--w-sidebar", key: "w-sidebar", axis: "x", min: 150, max: 600, sign: 1 },
  "grip-list":    { prop: "--w-list",    key: "w-list",    axis: "x", min: 240, max: 900, sign: 1 },
  // The categories panel grows upward, so dragging the grip down shrinks it.
  "cats-grip":    { prop: "--h-cats",    key: "h-cats",    axis: "y", min: 60,  max: 600, sign: -1 },
};

function wireGrips() {
  for (const [id, spec] of Object.entries(PANES)) {
    const el = document.getElementById(id);
    if (!el) continue;

    const saved = localStorage.getItem(spec.key);
    if (saved) document.documentElement.style.setProperty(spec.prop, saved);

    el.onpointerdown = (e) => {
      e.preventDefault();
      el.setPointerCapture(e.pointerId);
      el.classList.add("dragging");
      document.body.classList.add(spec.axis === "x" ? "resizing" : "resizing-v");

      const start = spec.axis === "x" ? e.clientX : e.clientY;
      const cur = parseFloat(
        getComputedStyle(document.documentElement).getPropertyValue(spec.prop)) || 0;

      const move = (ev) => {
        const now = spec.axis === "x" ? ev.clientX : ev.clientY;
        const next = Math.min(spec.max, Math.max(spec.min, cur + (now - start) * spec.sign));
        document.documentElement.style.setProperty(spec.prop, next + "px");
      };
      const up = () => {
        el.releasePointerCapture?.(e.pointerId);
        el.classList.remove("dragging");
        document.body.classList.remove("resizing", "resizing-v");
        el.removeEventListener("pointermove", move);
        el.removeEventListener("pointerup", up);
        localStorage.setItem(
          spec.key,
          getComputedStyle(document.documentElement).getPropertyValue(spec.prop).trim());
      };
      el.addEventListener("pointermove", move);
      el.addEventListener("pointerup", up);
    };

    // Keyboard, because a 4px target is not reachable for everyone.
    el.onkeydown = (e) => {
      const step = e.shiftKey ? 40 : 10;
      let d = 0;
      if (e.key === "ArrowLeft" || e.key === "ArrowUp") d = -step;
      else if (e.key === "ArrowRight" || e.key === "ArrowDown") d = step;
      else return;
      e.preventDefault();
      const cur = parseFloat(
        getComputedStyle(document.documentElement).getPropertyValue(spec.prop)) || 0;
      const next = Math.min(spec.max, Math.max(spec.min, cur + d * spec.sign));
      document.documentElement.style.setProperty(spec.prop, next + "px");
      localStorage.setItem(spec.key, next + "px");
    };

    el.ondblclick = () => {
      document.documentElement.style.removeProperty(spec.prop);
      localStorage.removeItem(spec.key);
    };
  }
}

// ------------------------------------------------------------------- layout
//
// Classic is the three-pane reader. Newspaper is one wide column of cards that
// expand in place, which is why it hides the reading pane rather than shrinking
// it: two copies of the same article side by side is not a layout.

function setLayout(l) {
  document.documentElement.dataset.layout = l;
  localStorage.setItem("layout", l);
  document.querySelectorAll("button[data-layout]").forEach((x) =>
    x.setAttribute("aria-pressed", String(x.dataset.layout === l)));
  // Leaving newspaper with a card open should land in the reading pane on the
  // same article rather than on "select an article".
  if (l !== "newspaper" && state.selected) openArticle(state.selected, { keepSelection: true });
  else if (l === "newspaper") loadList();
  else renderList();
}

// ------------------------------------------------------------------- labels
//
// The menu that puts labels on the selected articles. Ticks reflect the
// selection: a label is ticked when every selected article already has it, so
// clicking it takes it off all of them.

function labelMenuItems() {
  const ids = targetIds();
  const rows = itemsFor(ids);
  if (!state.labels.length) {
    return [{ label: "No labels yet", disabled: true },
            "-",
            { label: "Manage labels…", run: () => openSettings("labels") }];
  }
  const items = state.labels.map((l) => {
    const all = rows.length > 0 && rows.every((r) => (r.labels || []).includes(l.id));
    return {
      label: l.name,
      checked: all,
      disabled: !ids.length,
      run: async () => {
        try {
          await invoke("set_label", { ids, labelId: l.id, on: !all });
          await loadList();
          await loadTree();
        } catch (e) { toast(String(e)); }
      },
    };
  });
  const anyLabelled = rows.some((r) => (r.labels || []).length);
  items.push("-");
  items.push({
    label: "Clear labels", disabled: !anyLabelled,
    run: async () => {
      await invoke("clear_labels", { ids });
      await loadList(); await loadTree();
    },
  });
  items.push({ label: "Manage labels…", run: () => openSettings("labels") });
  return items;
}

/// Minimise-to-tray has to be done from the frontend: the window event fires
/// before the platform minimises, and hiding from there is the only way to get
/// the taskbar button to go away too.
let minimiseToTray = false;
/// The global settings as last loaded or saved, for behaviour that reads
/// them at the moment it happens (mark read on open, Enter opens browser).
let appSettings = null;
function applyTrayBehaviour(values) {
  appSettings = { ...SETTING_DEFAULTS, ...(appSettings || {}), ...values };
  minimiseToTray = appSettings["startup.minimize_to_tray"] === "1";
}
function settingOn(key) {
  return ((appSettings || SETTING_DEFAULTS)[key] ?? SETTING_DEFAULTS[key]) === "1";
}

// Theme and density persist in localStorage; both are per-machine preferences
// with no reason to live in the database.
function applyTheme(name) {
  document.documentElement.dataset.theme = name;
  localStorage.setItem("theme", name);
}
$("#btn-theme").onclick = () => {
  const cur = document.documentElement.dataset.theme || "system";
  applyTheme(THEMES[(THEMES.indexOf(cur) + 1) % THEMES.length]);
  toast(document.documentElement.dataset.theme);
};
$("#seg-density").onclick = (e) => {
  const b = e.target.closest("button"); if (!b) return;
  setDensity(b.dataset.density);
};
$("#seg-layout").onclick = (e) => {
  const b = e.target.closest("button"); if (!b) return;
  setLayout(b.dataset.layout);
};
$("#btn-settings").onclick = () => openSettings();
$("#btn-feedprops").onclick = () => openFeedSettings(currentFeedId());
// The article on screen, not whatever else is selected in the list.
$("#btn-readdelete").onclick = () => deleteArticles(state.selected ? [state.selected] : []);
$("#btn-labelmenu").onclick = (e) => {
  e.stopPropagation();
  const r = (toolbarAnchor || $("#btn-labelmenu")).getBoundingClientRect();
  showCtx(r.left, r.bottom + 4, labelMenuItems(), "Labels");
};

$("#cats-head").onclick = () => {
  const h = $("#cats-head");
  const open = h.getAttribute("aria-expanded") !== "true";
  h.setAttribute("aria-expanded", String(open));
  h.title = open ? "Collapse categories" : "Expand categories";
  localStorage.setItem("catsOpen", String(open));
};

// The search runs over the whole scope in the database, a moment after the
// last keystroke rather than on every one.
let searchTimer = null;
$("#q").oninput = (e) => {
  state.query = e.target.value;
  clearTimeout(searchTimer);
  searchTimer = setTimeout(() => { $("#list").scrollTop = 0; loadList(); }, 250);
};
// Ctrl + wheel over an article changes its text size, as in a browser.
for (const sel of ["#article", "#list"]) {
  $(sel).addEventListener("wheel", (e) => {
    if (!e.ctrlKey) return;
    if (sel === "#list" && !e.target.closest(".full")) return;
    e.preventDefault();
    if (e.deltaY < 0) textBigger(); else if (e.deltaY > 0) textSmaller();
  }, { passive: false });
}
$("#list").addEventListener("scroll", () => {
  const el = $("#list");
  if (el.scrollTop + el.clientHeight > el.scrollHeight - 600) loadMore();
});
document.querySelectorAll("#listhead [data-sort]").forEach((b) => {
  b.onclick = () => toggleSort(b.dataset.sort);
});
paintSortHead();

// ------------------------------------------------------------- shortcuts
// Every rebindable shortcut: what it is called, its default key, and what it
// does. The Shortcuts page edits `localStorage.keymap`, which holds only the
// keys changed from these defaults.
const KEY_ACTIONS = [
  ["next",        "Next article",                 "J",       () => step(1)],
  ["prev",        "Previous article",             "K",       () => step(-1)],
  ["extendNext",  "Extend the selection down",    "Shift+J", () => extendSelection(1)],
  ["extendPrev",  "Extend the selection up",      "Shift+K", () => extendSelection(-1)],
  ["openBrowser", "Open in browser",              "B",       () => $("#openext").click()],
  ["star",        "Star or unstar",               "S",       () => toggleStar()],
  ["toggleRead",  "Toggle read",                  "M",       () => $("#btn-toggleread").click()],
  ["markAllRead", "Mark all read",                "Shift+M", () => $("#btn-listmarkall").click()],
  ["labels",      "Labels",                       "L",       () => openLabelMenu()],
  ["delete",      "Delete",                       "Delete",  () => deleteByFocus()],
  ["selectAll",   "Select all",                   "Ctrl+A",  () => selectAllVisible()],
  ["undo",        "Undo the last delete",         "Ctrl+Z",  () => undo()],
  ["search",      "Search",                       "/",       () => $("#q").focus()],
  ["update",      "Update all",                   "F5",      () => $("#btn-update").click()],
  ["addFeed",     "Add feed",                     "Ctrl+N",  () => addFeed()],
  ["settings",    "Settings",                     "Ctrl+,",  () => openSettings()],
  ["textBigger",  "Larger text",                  "Ctrl+=",  () => textBigger()],
  ["textSmaller", "Smaller text",                 "Ctrl+-",  () => textSmaller()],
  ["textReset",   "Normal text size",             "Ctrl+0",  () => textReset()],
  ["quit",        "Exit",                         "Ctrl+Q",  () => invoke("quit_app")],
];
const KEY_GROUPS = [
  ["READING", ["next", "prev", "openBrowser", "textBigger", "textSmaller", "textReset"]],
  ["MARKING", ["star", "toggleRead", "labels", "markAllRead", "delete", "undo"]],
  ["SELECTION", ["extendNext", "extendPrev", "selectAll"]],
  ["ELSEWHERE", ["search", "update", "addFeed", "settings", "quit"]],
];
/// Second keys that come free with a default: Ctrl and the + key, which
/// is Shift and = on most keyboards, or the number pad's +.
const KEY_EXTRAS = { textBigger: ["Ctrl++"] };

/// The key that was pressed, as the keymap writes it: "Ctrl+Shift+J", "F5".
/// Shift is named only where it does not already change the character, so
/// Shift+= arrives as "+". The Windows key is its own modifier, not Ctrl:
/// Win+Shift+S (the screenshot tool) starred the open article when it was
/// read as a plain S.
function comboOf(e) {
  let k = e.key;
  if (!k || ["Control", "Shift", "Alt", "Meta", "OS", "AltGraph"].includes(k)) return null;
  if (k === " ") k = "Space";
  if (k.length === 1) k = k.toUpperCase();
  const mods = [];
  if (e.ctrlKey) mods.push("Ctrl");
  if (e.altKey) mods.push("Alt");
  if (e.metaKey) mods.push("Win");
  if (e.shiftKey && (/^[A-Z0-9]$/.test(k) || k.length > 1)) mods.push("Shift");
  return [...mods, k].join("+");
}

function keymap() {
  let saved = {};
  try { saved = JSON.parse(localStorage.getItem("keymap") || "{}") || {}; } catch {}
  const map = {};
  for (const [id, , def] of KEY_ACTIONS) map[id] = id in saved ? saved[id] : def;
  return map;
}
function saveKeymap(map) {
  const changed = {};
  for (const [id, , def] of KEY_ACTIONS) if (map[id] !== def) changed[id] = map[id];
  try { localStorage.setItem("keymap", JSON.stringify(changed)); } catch {}
  refreshKeyTips();
}
function actionFor(combo) {
  const map = keymap();
  for (const [id] of KEY_ACTIONS) if (map[id] && map[id] === combo) return id;
  // Second keys only after every chosen one: a key the user gave to another
  // action belongs to that action.
  for (const [id, , def] of KEY_ACTIONS) {
    if ((KEY_EXTRAS[id] || []).includes(combo) && map[id] === def) return id;
  }
  return null;
}
/// How a key reads in menus and tooltips.
function keyLabel(combo) {
  return (combo || "").replace(/(^|\+)Delete$/, "$1Del");
}
function keyHint(id) { return keyLabel(keymap()[id]) || undefined; }

/// Tooltips name the shortcut, and follow it when it is changed. Buttons are
/// matched by the default key their tooltip was written with.
function refreshKeyTips() {
  const byDefault = Object.fromEntries(KEY_ACTIONS.map(([id, , def]) => [keyLabel(def), id]));
  const map = keymap();
  document.querySelectorAll("[title]").forEach((el) => {
    if (!el.dataset.keyaction) {
      const m = el.title.match(/^(.*) \(([^()]+)\)$/);
      if (!m || !byDefault[m[2]]) return;
      el.dataset.keyaction = byDefault[m[2]];
      el.dataset.tip = m[1];
    }
    const k = keyLabel(map[el.dataset.keyaction]);
    el.title = k ? `${el.dataset.tip} (${k})` : el.dataset.tip;
  });
}

function extendSelection(dir) {
  const items = visibleItems();
  const idx = items.findIndex((i) => i.id === state.selected);
  // With nothing open, start at the end being extended from.
  const next = idx < 0 ? items[dir > 0 ? 0 : items.length - 1] : items[idx + dir];
  if (next) {
    const sel = new Set(state.sel);
    sel.add(next.id);
    setSelection(sel);
    openArticle(next.id, { keepSelection: true });
  }
}

function openLabelMenu() {
  // Anchored to the list selection rather than to the button, because the
  // button can be hidden by a customised toolbar.
  const row = document.querySelector('.item[aria-selected="true"]')
           || document.querySelector(".item");
  const r = (row || $("#listbar")).getBoundingClientRect();
  showCtx(r.left + 40, r.top + 20, labelMenuItems(), "Labels");
}

function deleteByFocus() {
  // With a feed selected in the tree, Delete means that feed. Otherwise it
  // means the articles. Previously it always meant the articles, so
  // pressing Delete over the tree quietly deleted whatever was open.
  // The row with keyboard focus, which need not be the selected one.
  const focused = document.activeElement?.closest?.("#tree .node");
  if (focused) removeNode(focused);
  else if (document.activeElement?.closest("#tree")) $("#btn-removefeed").click();
  else $("#btn-delete").click();
}

// Capturing, and registered before Settings' own Esc handler, so a key
// being recorded reaches the Shortcuts page and nothing else.
document.addEventListener("keydown", (e) => { if (keyCapture) keyCapture(e); }, true);

/// Whether a dialog or sheet is open. They are all added straight to
/// <body>, so only its own children are looked at: searching the whole page
/// for one on every key press took milliseconds with thousands of articles
/// loaded.
function dialogUp() {
  for (const el of document.body.children) {
    if (el.classList.contains("modal-back") || el.classList.contains("sheet-back")
        || el.getAttribute("aria-modal") === "true") return true;
  }
  return false;
}

document.addEventListener("keydown", (e) => {
  if (keyCapture) return;
  // Handled already, by the focused tree row for one: Space there opened the
  // feed and then, bound to "Next article", moved on as well.
  if (e.defaultPrevented) return;
  const dialog = dialogUp();
  const typing = /^(INPUT|TEXTAREA|SELECT)$/.test(e.target.tagName) || e.target.isContentEditable;
  // Esc in a dialog's field closes the dialog; elsewhere it leaves the field.
  if (typing && !(dialog && e.key === "Escape")) { if (e.key === "Escape") e.target.blur(); return; }
  const combo = comboOf(e);
  const action = combo && actionFor(combo);

  // Settings, feed properties and every dialog own the keyboard while open.
  // Without this, a key pressed in a dialog also acted on the article list
  // behind it: S starred, Delete deleted, Enter opened the browser.
  if (dialog) {
    if (action === "quit") { e.preventDefault(); invoke("quit_app"); }
    // Esc closes the topmost dialog. Settings handles its own; this covers
    // Feed properties and dialogs opened from the sidebar.
    if (e.key === "Escape") {
      const top = [...document.querySelectorAll(".modal-back")].pop();
      if (top) top.click();
      else if (sheetEl) closeSheet();
    }
    return;
  }

  if (action) {
    // Enter and Delete on a focused button or menu belong to that element.
    e.preventDefault();
    KEY_ACTIONS.find((a) => a[0] === action)[3]();
    return;
  }

  // Not rebindable: Escape and Enter keep their usual meanings.
  if (e.key === "Escape" && state.sel.size > 1) {
    setSelection(state.selected ? [state.selected] : []);
    return;
  }
  if (combo === "Enter") {
    // Enter on a button, a tree row or a menu item belongs to that element;
    // opening the article as well sent it to the browser twice, or on top
    // of switching feeds.
    const t = e.target;
    const plain = t === document.body || t.closest?.("#list, #article");
    if (plain && !t.closest?.("button, a, [role=button], select") && settingOn("reading.enter_opens_browser")) {
      $("#openext").click();
    }
  }
});

// A filter's sound, when the app could not play it itself (on Windows it
// plays .wav files directly).
listen("play-sound", async (e) => {
  try {
    const bytes = await invoke("read_sound", { path: e.payload });
    const url = URL.createObjectURL(new Blob([bytes]));
    const audio = new Audio(url);
    audio.onended = audio.onerror = () => URL.revokeObjectURL(url);
    await audio.play();
  } catch (err) {
    console.warn("sound", err);
  }
});

// The backend paints the feed summary at once and fetches the real article
// behind it. When it lands, swap it in — but only if the pane is still showing
// that article, or a slow fetch would overwrite whatever the user moved on to.
listen("article-ready", async (e) => {
  const { id, ok } = e.payload || {};
  if (id !== state.selected) return;
  if (!ok) {
    // The feed's summary is what there is; stop saying more is coming.
    $(".pendingchip")?.remove();
    return;
  }
  try {
    const a = await invoke("article", { id });
    if (id !== state.selected) return;
    state.current = a;
    const prose = $("#article .prose");
    if (prose) {
      prose.innerHTML = a.html;
      prose.querySelectorAll("a[href]").forEach((link) => {
        link.onclick = (ev) => { ev.preventDefault(); openUrl(link.href); };
      });
    }
    $(".pendingchip")?.remove();
    if (document.documentElement.dataset.layout === "newspaper") redrawRows([id]);
  } catch { /* the summary stays */ }
});

// ------------------------------------------------------------------- updates

async function checkForUpdates() {
  toast("Checking for updates…");
  try {
    const info = await invoke("check_update");
    if (info) showUpdateBar(info);
    else toast(`Version ${APP_VERSION} is the newest`);
  } catch (e) {
    toast(String(e));
    if (String(e).includes("Settings")) openSettings("updates");
  }
}

/// A bar across the top: the new version, install, or later. Not a dialog,
/// because a daily check should not interrupt reading.
function showUpdateBar(info) {
  $("#updatebar")?.remove();
  const bar = document.createElement("div");
  bar.id = "updatebar";
  bar.innerHTML = `<span data-msg>SnapRSS ${esc(info.version)} is available.</span>
    <button class="pill" data-install>Install and restart</button>
    <button class="pill" data-later>Later</button>`;
  document.body.append(bar);
  bar.querySelector("[data-later]").onclick = () => bar.remove();
  bar.querySelector("[data-install]").onclick = async () => {
    bar.querySelectorAll("button").forEach((b) => (b.disabled = true));
    bar.querySelector("[data-msg]").textContent = "Downloading…";
    try {
      await invoke("install_update");
    } catch (e) {
      // The found update is used up by the attempt; a new check is needed.
      bar.querySelector("[data-msg]").textContent = String(e);
      bar.querySelector("[data-install]").remove();
      const later = bar.querySelector("[data-later]");
      later.disabled = false;
      later.textContent = "Close";
    }
  };
}

listen("update-available", (e) => { if (e.payload) showUpdateBar(e.payload); });
listen("update-download", (e) => {
  const { got, total } = e.payload || {};
  const msg = $("#updatebar [data-msg]");
  if (msg) msg.textContent = total ? `Downloading… ${Math.floor((got / total) * 100)}%` : "Downloading…";
});

// Live progress while feeds update.
listen("tray-update", () => $("#btn-update").click());

listen("update-progress", (e) => {
  const p = e.payload || {};
  // The last fetch finishing is not the update finishing: results are still
  // being written, and a manual update is done when its call returns.
  if (manualUpdate) setUpdating(true, p);
  else setUpdating(p.done < p.total, p);
});

listen("icons-updated", async () => {
  await loadIcons();
  await loadTree();
  refreshRowIcons();
});

listen("feeds-updated", async (e) => {
  await loadTree();
  await loadList();
  await refreshStatus();
  if (e.payload) toast(`${e.payload} new`);
});

async function refreshStatus() {
  const c = await invoke("counts");
  $("#st-counts").textContent = `${c.unread} unread · ${c.total} articles`;
}

(async function start() {
  applyTheme(localStorage.getItem("theme") || "system");
  setTextScale(textScale(), { quiet: true });
  const d = localStorage.getItem("density") || "relaxed";
  document.documentElement.dataset.density = d;
  $("#seg-density").querySelectorAll("button").forEach((x) =>
    x.setAttribute("aria-pressed", String(x.dataset.density === d)));

  const l = localStorage.getItem("layout") || "classic";
  document.documentElement.dataset.layout = l;
  $("#seg-layout").querySelectorAll("button").forEach((x) =>
    x.setAttribute("aria-pressed", String(x.dataset.layout === l)));

  seedToolbarClones();
  applyToolbars();
  refreshKeyTips();
  wireGrips();
  wireTreeRootDrop();

  if (localStorage.getItem("catsOpen") === "false")
    $("#cats-head").setAttribute("aria-expanded", "false");

  $("#scopename").textContent = state.scopeName;

  // Content first. Icons are one quick read, needed by both panes. The tree
  // and the list do not depend on each other, so they load together; the
  // list is drawn once more afterwards so its label chips can use the labels
  // the tree brought back.
  try {
    await loadIcons();
    await Promise.all([loadTree(), loadList(), refreshStatus()]);
    renderList();
  } finally {
    // The window was placed while hidden; it appears now, with its content.
    invoke("window_ready").catch(() => {});
  }
  setInterval(refreshStatus, 15000);

  // Then the plumbing that has no effect on the first paint. The backend
  // already applied close-to-tray from the database at launch.
  invoke("get_settings")
    .then((v) => applyTrayBehaviour({ ...SETTING_DEFAULTS, ...v }))
    .catch(() => {});
  try {
    getCurrentWindow().onResized(async () => {
      if (minimiseToTray && (await getCurrentWindow().isMinimized())) {
        await getCurrentWindow().hide();
      }
    })?.catch?.(() => {});
  } catch { /* no window API in the harness */ }
})();
