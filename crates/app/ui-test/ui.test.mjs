// Runs ui/index.html + ui/app.js in jsdom with a stubbed Tauri bridge, then
// drives the buttons. Catches the class of bug that makes the window render
// but nothing respond.
//
//   node ui.test.mjs
//
// Not a substitute for running the app, but it executes every code path the
// frontend takes on startup and on each toolbar button.

import { JSDOM, VirtualConsole } from "jsdom";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const UI = process.env.UI_DIR || join(here, "..", "ui");

let failures = 0;
const ok = (cond, label) => {
  console.log(`${cond ? "  ok  " : " FAIL "} ${label}`);
  if (!cond) failures++;
};

// ---------------------------------------------------------------- fake backend
const calls = [];
const TREE = [
  {
    id: 1, is_folder: true, parent_id: null, title: "Engineering",
    unread: 3, broken: false, expanded: true,
    children: [
      { id: 2, is_folder: false, parent_id: 1, title: "Kernel Notes", unread: 2, broken: false, children: [] },
      { id: 3, is_folder: false, parent_id: 1, title: "Cold Storage", unread: 1, broken: true, children: [] },
    ],
  },
];
const ITEMS = [
  { id: 10, feed_id: 2, feed_title: "Kernel Notes", title: "First post", author: "M. Vogel",
    published: new Date().toISOString(), link: "https://kn.test/1", read: false, starred: false,
    excerpt: "An excerpt of the first post, long enough to wrap.", labels: [1] },
  { id: 11, feed_id: 2, feed_title: "Kernel Notes", title: "Second post", author: null,
    published: new Date(Date.now() - 86400000).toISOString(), link: "https://kn.test/2", read: true, starred: true,
    excerpt: "Second excerpt.", labels: [1, 2] },
  { id: 12, feed_id: 2, feed_title: "Kernel Notes", title: "Third post", author: null,
    published: new Date(Date.now() - 2 * 86400000).toISOString(), link: "https://kn.test/3", read: false, starred: false,
    excerpt: "Third excerpt.", labels: [] },
  { id: 13, feed_id: 3, feed_title: "Cold Storage", title: "Fourth post", author: null,
    published: new Date(Date.now() - 3 * 86400000).toISOString(), link: "https://kn.test/4", read: false, starred: false,
    excerpt: "Fourth excerpt.", labels: [] },
];

const autostart = { value: false };

const FILTERS = [
  { id: 1, name: "Drop sponsored", mode: 1, enabled: true, feeds: null, broken: false,
    conditions: [{ field: "title", op: "contains", content: "sponsored" }],
    actions: [{ action: "delete", params: null }] },
  { id: 2, name: "Star Vogel", mode: 1, enabled: false, feeds: [2], broken: false,
    conditions: [{ field: "author", op: "contains", content: "vogel" }],
    actions: [{ action: "add_star", params: null }] },
];

// Per-call delays, so tests can make replies arrive out of order.
const delay = { article: {}, news_list: {}, get_settings: 0, update_all: 0 };
const upd = { result: null, error: null, installError: null };
const fail = { article: new Set() };
const ivType = { value: "minutes" };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function invoke(cmd, args) {
  calls.push([cmd, args]);
  switch (cmd) {
    case "feed_tree": return structuredClone(TREE);
    case "feed_icons": return structuredClone(icons.value);
    case "refresh_feed_icon": return false;
    case "counts": return { ...countsNow.value };
    case "news_list":
      if (delay.news_list[args.scope]) await sleep(delay.news_list[args.scope]);
      // "scope@offset" delays one page, so a later page can land first.
      if (delay.news_list[`${args.scope}@${args.offset}`]) await sleep(delay.news_list[`${args.scope}@${args.offset}`]);
    {
      // Like the backend: every search word must appear in the title,
      // author or feed name; sorted; then the requested page.
      let rows = args.scope === "label:77" || (args.scope === "unread" && bigUnread.value)
        ? Array.from({ length: bigRows.value }, (_, n) => ({ ...structuredClone(ITEMS[0]), id: 5000 + n,
            title: `Old post ${n}`, published: new Date(BIG_T0 - n * 3600e3).toISOString(), labels: [] }))
        : structuredClone(ITEMS).map((i) => ({ ...i, title: delay.news_list[args.scope] ? `${args.scope} ${i.title}` : i.title }));
      const words = (args.query || "").toLowerCase().split(/\s+/).filter(Boolean);
      rows = rows.filter((i) => words.every((w) =>
        [i.title, i.author || "", i.feed_title || ""].some((f) => f.toLowerCase().includes(w))));
      const sort = args.sort || "-date";
      const key = sort.replace(/^-/, "");
      const get = (i) => key === "title" ? i.title.toLowerCase() : key === "author" ? (i.author || "")
        : key === "feed" ? (i.feed_title || "") : i.published;
      rows.sort((a, b) => (get(a) < get(b) ? -1 : get(a) > get(b) ? 1 : 0) * (sort.startsWith("-") ? -1 : 1));
      const off = args.offset || 0;
      return rows.slice(off, off + (args.limit || 500));
    }
    case "article":
      if (delay.article[args.id]) await sleep(delay.article[args.id]);
      if (fail.article.has(args.id)) throw "boom";
      return {
      id: args.id, title: args.id === 10 ? "First post" : `Article ${args.id}`, byline: "M. Vogel", feed_title: "Kernel Notes",
      published: new Date().toISOString(), link: "https://kn.test/1",
      html: "<p>Body text with a <a href='https://out.test/x'>link</a>.</p>",
      read_minutes: 4, starred: false, from_feed: false, pending: false,
    };
    // Like the backend: every unread id in the scope it marked.
    case "mark_scope_read": return ITEMS.filter((i) => !i.read).map((i) => i.id);
    case "set_read": case "set_starred": case "set_deleted":
    case "rename_node": case "remove_feed":
    case "set_reading_mode": return null;
    case "add_feed": return 42;
    // Like the backend: a feed address is itself; a site lists its feeds.
    case "discover_feed": {
      const a = args.address;
      if (a.includes("none.test")) return [];
      if (a.includes("locked.test")) throw "could not open https://locked.test/rss: http 401: this feed needs a user name and password";
      if (a.includes("down.test")) throw "could not open https://down.test/: network: connection refused";
      if (a.includes("multi.test")) return [
        { url: "https://multi.test/posts.xml", title: "Posts", subscribed: true },
        { url: "https://multi.test/comments.xml", title: "Comments", subscribed: false },
      ];
      if (a.includes("kn.test")) return [{ url: "https://kn.test/feed.xml", title: "Kernel Notes", subscribed: true }];
      return [{ url: a.includes("://") ? a : `https://${a}`, title: null, subscribed: false }];
    }
    case "add_folder": return 43;
    case "update_all": if (delay.update_all) await sleep(delay.update_all); return { attempted: 2, not_modified: 1, ingested: 1, failed: 0, new_articles: 5 };
    case "update_feed_now": return { attempted: 1, not_modified: 0, ingested: 1, failed: 0, new_articles: 2 };
    case "import_file": return { kind: "opml", folders: 2, feeds: 12, news: 0,
                                 labels: 0, filters: 0, duplicates: 1, skipped: [] };
    case "export_opml": return 12;
    case "labels": return [
      { id: 1, name: "Important", color_bg: "#B33A35", count: 3 },
      { id: 2, name: "Read later", color_bg: "#3B6EA5", count: 8 },
    ];
    case "get_settings": if (delay.get_settings) await sleep(delay.get_settings); return {
      "update.interval_minutes": "15",
      "cleanup.max_to_keep": "",
      "cleanup.never_delete_unread": "1",
    };
    case "set_settings": return null;
    case "db_stats": return {
      bytes: 2_097_152, feeds: 12, articles: 269, unread: 267, starred: 0,
      deleted: 4, with_cached_article: 5, oldest: "2015-01-03T00:00:00Z",
    };
    case "run_cleanup": return { feeds: 12, by_read: 0, by_age: 0, by_count: 9, purged: 0, bytes_freed: 0 };
    case "purge_deleted": return { feeds: 0, by_read: 0, by_age: 0, by_count: 0, purged: 4, bytes_freed: 1024 };
    case "vacuum_db": return 4096;
    case "clear_article_cache": return 5;
    case "backup_db": return 2_097_152;
    case "feed_settings": return {
      id: args.id, title: "Kernel Notes", xmlUrl: "https://kn.test/feed.xml",
      htmlUrl: "https://kn.test", descriptionOnly: false, loadImages: true,
      saveOffline: false, showNotification: false, disableUpdate: false,
      layoutDirection: 0, updateIntervalEnable: false, updateInterval: 15,
      updateIntervalType: ivType.value, duplicateNewsMode: false,
      maxToKeepEnable: false, maxToKeep: null, maxAgeEnable: false, maxAgeDays: null,
      deleteRead: false, neverDeleteUnread: true, neverDeleteStarred: true,
      neverDeleteLabeled: true, status: "", updated: null, articleCount: 40,
      ...feedSignIn.value,
    };
    case "save_feed_settings": return null;
    case "check_update": if (upd.error) throw upd.error; return upd.result;
    case "install_update": if (upd.installError) throw upd.installError; return null;
    case "apply_settings_to_folder": return 11;
    case "save_label": return 7;
    case "delete_label": case "set_label": case "clear_labels":
    case "reorder_label": return null;
    case "filters": return structuredClone(FILTERS);
    case "filter_vocabulary": return {
      fields: ["title", "description", "author", "category", "status", "link", "news"],
      ops: {
        title: ["contains", "not_contains", "is", "is_not", "begins_with", "ends_with", "regex"],
        description: ["contains", "not_contains", "regex"],
        author: ["contains", "not_contains", "is", "is_not", "regex"],
        category: ["contains", "not_contains", "is", "is_not", "begins_with", "ends_with", "regex"],
        status: ["is", "is_not"],
        link: ["contains", "not_contains", "is", "is_not", "begins_with", "ends_with", "regex"],
        news: ["contains", "not_contains", "regex"],
      },
      statuses: ["new", "read", "starred"],
      actions: ["mark_read", "add_star", "delete", "add_label", "play_sound", "notify"],
    };
    case "test_sound": return null;
    case "read_sound": return new Uint8Array([82, 73, 70, 70]).buffer;
    case "save_filter": return 3;
    case "delete_filter": case "set_filter_enabled": case "reorder_filter": return null;
    case "apply_filters_now": return {
      considered: 40, matched: 6, marked_read: 4, starred: 1, deleted: 1, labelled: 2,
    };
    case "move_node": return null;
    case "set_expanded": TREE.find((n) => n.id === args.id).expanded = args.expanded; return null;
    case "autostart_enabled": return autostart.value;
    case "set_autostart": autostart.value = args.on; return args.on;
    case "set_close_to_tray": return null;
    case "quit_app": return null;
    default: throw new Error(`unknown command ${cmd}`);
  }
}

// ------------------------------------------------- configuration invariants
//
// The harness below evaluates app.js directly, so it cannot catch a Content
// Security Policy that blocks the script, or a missing Tauri global. Those are
// exactly the two mistakes that shipped a window where nothing responded, so
// they are checked statically here instead.

console.log("configuration");

/// A missing or unreadable file has to fail an assertion, not throw. Crashing
/// here exits before a single DOM test runs, and a grep for FAIL then finds
/// nothing — which reads exactly like a pass.
function readOr(path, fallback, label) {
  try {
    return readFileSync(path, "utf8");
  } catch (e) {
    ok(false, `could not read ${label}: ${e.code || e.message}`);
    return fallback;
  }
}
function readJsonOr(path, fallback, label) {
  try {
    return JSON.parse(readOr(path, "null", label)) ?? fallback;
  } catch (e) {
    ok(false, `${label} is not valid JSON: ${e.message}`);
    return fallback;
  }
}

const CONF = readJsonOr(join(UI, "..", "tauri.conf.json"), { app: {} }, "tauri.conf.json");
const rawHtml = readOr(join(UI, "index.html"), "", "index.html");

ok(CONF.app?.withGlobalTauri === true,
   "app.withGlobalTauri is true, so window.__TAURI__ exists");

const csp = CONF.app?.security?.csp || "";
const scriptSrc = (csp.match(/script-src([^;]*)/) || [, ""])[1];
const inlineScripts = [...rawHtml.matchAll(/<script(?![^>]*\bsrc=)[^>]*>([\s\S]*?)<\/script>/g)]
  .filter((m) => m[1].trim().length > 0);
ok(!(scriptSrc && !scriptSrc.includes("unsafe-inline") && inlineScripts.length > 0),
   `no inline <script> blocks under script-src${scriptSrc} (found ${inlineScripts.length})`);

ok(/<script[^>]*\bsrc=["']app\.js["']/.test(rawHtml), "index.html loads app.js as a file");

// Small grey text (dates, captions, counts, read titles) and accent-coloured
// text stay at 4.5:1 on every panel of every light theme.
{
  const lum = (h) => {
    const c = [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16) / 255)
      .map((v) => (v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4));
    return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
  };
  const ratio = (a, b) => { const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p); return (x + 0.05) / (y + 0.05); };
  const block = (sel) => { const m = rawHtml.match(new RegExp(sel.replace(/[[\]"]/g, "\\$&") + "\\s*\\{([^}]*)\\}")); return m ? m[1] : ""; };
  const tok = (body, k) => (body.match(new RegExp("--" + k + ":\\s*(#[0-9A-Fa-f]{6})")) || [])[1];
  const base = block(":root");
  const worst = [];
  for (const t of ["system", "system2", "gray", "green", "orange", "pink", "purple"]) {
    const b = t === "system" ? "" : block(`:root[data-theme="${t}"]`);
    const get = (k) => tok(b, k) || tok(base, k);
    for (const fg of ["ink-4", "accent-text"]) {
      for (const bg of ["chrome", "bar", "sidebar", "list", "page", "sel"]) {
        const r = ratio(get(fg), get(bg));
        if (r < 4.5) worst.push(`${t} ${fg} on ${bg}: ${r.toFixed(2)}`);
      }
    }
  }
  ok(worst.length === 0, `small text contrast is at least 4.5:1 in light themes ${worst.join("; ")}`);
}

const appSrc = readOr(join(UI, "app.js"), "", "app.js");
ok(!/(^|[^.\w])prompt\s*\(/.test(appSrc),
   "no window.prompt (unimplemented in WebView2)");
ok(!/(^|[^.\w])confirm\s*\(/.test(appSrc),
   "no window.confirm (unreliable across Tauri platforms)");

const perms = readJsonOr(join(UI, "..", "capabilities", "default.json"),
                         { permissions: [] }, "capabilities/default.json").permissions;
const permId = (p) => (typeof p === "string" ? p : p.identifier);
for (const want of ["opener:allow-open-url", "dialog:allow-open", "dialog:allow-save",
                    "clipboard-manager:allow-write-text"])
  ok(perms.some((p) => permId(p) === want), `capability ${want} granted`);

// Granting the command is not enough: without an allow scope the opener plugin
// rejects every URL with "Not allowed to open url".
const opener = perms.find((p) => permId(p) === "opener:allow-open-url");
const scoped = typeof opener === "object" && Array.isArray(opener.allow) &&
  ["https://*", "http://*"].every((u) => opener.allow.some((a) => a.url === u));
ok(scoped, "opener:allow-open-url has an http/https URL scope");

// Plain http images are common in feeds and were being blocked outright.
const conf_csp = CONF.app?.security?.csp || "";
const imgSrc = (conf_csp.match(/img-src([^;]*)/) || [, ""])[1];
for (const scheme of ["https:", "http:", "data:"])
  ok(imgSrc.includes(scheme), `img-src allows ${scheme}`);

// Tauri intercepts drag and drop at the OS level by default, which on Windows
// stops HTML5 drag and drop working inside the page at all. Its own schema
// says: "Disabling it is required to use HTML5 drag and drop on the frontend
// on Windows." WebKitGTK does not interfere, so the tree dragged fine on Linux
// and not at all in WebView2.
const win0 = (CONF.app?.windows || [])[0] || {};
ok(win0.dragDropEnabled === false,
   `dragDropEnabled is off, or in-page drag and drop dies on Windows (${win0.dragDropEnabled})`);

// The Windows taskbar showed the generic application icon. Tauri builds the
// window icon from the *first* entry of icon.ico and sets only ICON_SMALL; the
// taskbar reads ICON_BIG. main.rs now sets both from the embedded resource,
// and icon.ico must carry the sizes Windows asks for at each DPI, with the
// first entry large enough not to be a blurry 16px upscaled.
{
  let ico = null;
  try { ico = readFileSync(join(UI, "..", "icons", "icon.ico")); } catch {}
  ok(!!ico, "icons/icon.ico is readable");
  if (ico) {
    const n = ico.readUInt16LE(4);
    const sizes = [];
    for (let i = 0; i < n; i++) sizes.push(ico[6 + 16 * i] || 256);
    ok(sizes[0] >= 32, `icon.ico's first entry is not the 16px one (${sizes[0]})`);
    for (const need of [16, 24, 32, 48, 256])
      ok(sizes.includes(need), `icon.ico has a ${need}px image (${sizes.join(",")})`);
  }
  const mainRs = readOr(join(UI, "..", "src", "main.rs"), "", "main.rs");
  ok(/WM_SETICON[\s\S]*ICON_BIG/.test(mainRs) || /ICON_BIG[\s\S]*WM_SETICON/.test(mainRs),
     "the taskbar (ICON_BIG) icon is set explicitly on Windows");
  // The startup entry used to pass --minimized, which started the window
  // minimised whether or not "Start minimised" was on.
  ok(!/autostart::init\([^)]*--minimized/.test(mainRs) && !/args\(\)[\s\S]{0,40}--minimized/.test(mainRs),
     "starting with the system does not force a minimised window");
  // Hotlink-protected image hosts (image.gcores.com) 403 the webview's
  // tauri.localhost Referer and serve the same request without one.
  ok(/<meta name="referrer" content="no-referrer">/.test(readFileSync(join(UI, "index.html"), "utf8")),
     "images are requested without a Referer");
  // A second newspaper rule once fixed the sidebar at 268px and put the list
  // in the grip's column.
  ok(!/newspaper"\] #body \{ grid-template-columns: 268px/.test(readFileSync(join(UI, "index.html"), "utf8")),
     "the newspaper layout keeps the three-column grid");
  // The window comes back the size, place and maximised state it was left in.
  // Windows' own record of the restored rectangle and the maximised state;
  // the window-state plugin saved (-8,-8) as the position of a maximised one.
  ok(/GetWindowPlacement/.test(mainRs) && /SetWindowPlacement/.test(mainRs)
     && /WPF_RESTORETOMAXIMIZED/.test(mainRs) && !/tauri_plugin_window_state/.test(mainRs),
     "window size, position and maximised state are remembered");
}

// Every window call the frontend makes needs a matching permission, and jsdom
// cannot tell: "Command plugin:window|hide not allowed by ACL" only appears in
// the real app. Tauri's default window set is read-only getters, so anything
// that changes the window has to be granted by name.
{
  const js0 = readOr(join(UI, "app.js"), "", "app.js");
  const granted = new Set(perms.map(permId));
  const used = [...new Set([...js0.matchAll(/getCurrentWindow\(\)\.([a-zA-Z]+)\(/g)].map((m) => m[1]))];
  const readOnly = (m) => /^(is|inner|outer|scaleFactor|title|theme|on[A-Z])/.test(m);
  const kebab = (m) => m.replace(/[A-Z]/g, (c) => "-" + c.toLowerCase());
  for (const m of used) {
    if (readOnly(m)) continue;
    const need = `core:window:allow-${kebab(m)}`;
    ok(granted.has(need), `window.${m}() is permitted (${need})`);
  }
}

{
  const cargo = readOr(join(UI, "..", "..", "..", "Cargo.toml"), "", "Cargo.toml");
  const cv = (cargo.match(/^version\s*=\s*"([^"]+)"/m) || [])[1];
  ok(CONF.version === "1.0.0", `tauri.conf.json is version 1.0.0 (${CONF.version})`);
  ok(!cv || cv === CONF.version, `Cargo.toml agrees (${cv})`);
  let logo = null; try { logo = readFileSync(join(UI, "logo.png")); } catch {}
  ok(!!logo && logo.slice(1, 4).toString() === "PNG", "ui/logo.png ships with the frontend");
}

// ---------------------------------------------------------------------- set up
const vc = new VirtualConsole();
const consoleErrors = [];
vc.on("jsdomError", (e) => consoleErrors.push(String(e.message || e)));
vc.on("error", (...a) => consoleErrors.push(a.join(" ")));

const html = readFileSync(join(UI, "index.html"), "utf8");
const dom = new JSDOM(html, {
  runScripts: "outside-only",
  url: "http://localhost/",
  virtualConsole: vc,
  pretendToBeVisual: true,
});
const { window } = dom;
const doc = window.document;

const listeners = {};
const fire = (name, payload) =>
  Promise.all((listeners[name] || []).map((cb) => cb({ payload })));
const opened = [];
const picked = { value: "C:\\Users\\billy\\test.opml" };
const saved = { value: "C:\\Users\\billy\\snaprss.opml" };
const feedSignIn = { value: {} };
const bigUnread = { value: false };
const bigRows = { value: 1234 };
// Fixed, so the same article comes back the same from one request to the next.
const BIG_T0 = Date.now();
const countsNow = { value: { unread: 3, total: 9, starred: 1 } };
const icons = { value: { 2: "data:image/png;base64,iVBORw0KGgoAAAANSUhEUg==" } };
const closed = [];
const hidden = [];
window.__TAURI__ = {
  core: { invoke },
  app: { getVersion: async () => "1.0.0" },
  event: {
    listen: async (name, cb) => {
      (listeners[name] ||= []).push(cb);
      return () => {};
    },
  },
  opener: { openUrl: async (u) => opened.push(u) },
  dialog: { open: async () => picked.value, save: async () => saved.value },
  window: {
    getCurrentWindow: () => ({
      close: () => closed.push(true),
      hide: () => hidden.push(true),
      isMinimized: async () => false,
      onResized: async () => () => {},
    }),
  },
};
window.structuredClone = structuredClone;

// The page links app.js; jsdom will not fetch it, so evaluate it by hand the
// same way a module script would be evaluated.
// The trailing line gives tests a way to read the app's own `state`, which an
// indirect eval keeps out of the global scope.
const js = readFileSync(join(UI, "app.js"), "utf8");
try {
  window.eval(js + "\n;window.__appEval = (s) => eval(s);");
} catch (e) {
  console.log(` FAIL  app.js threw on load: ${e.message}`);
  process.exit(1);
}

const tick = (ms = 30) => new Promise((r) => setTimeout(r, ms));
const $ = (s) => doc.querySelector(s);
const click = (sel) => { const el = $(sel); if (!el) throw new Error(`no element ${sel}`); el.click(); };

// ----------------------------------------------------------------------- tests
await tick(80);

console.log("\nstartup");
ok($("#tree").children.length > 0, "feed tree rendered");
ok($("#catlist").children.length > 0, "categories panel rendered");
ok(doc.body.textContent.includes("Kernel Notes"), "a feed name appears in the tree");
ok($("#st-counts").textContent.includes("unread"), "status bar populated");
ok(calls.some(([c]) => c === "feed_tree"), "feed_tree was invoked");
ok(!doc.body.textContent.includes("Tauri bridge is missing"), "bridge guard did not trip");

// Nothing is chosen for the user: the list stays empty until a feed, folder
// or category is clicked, and stays empty through an update.
ok(doc.querySelectorAll("#list .item").length === 0, "the list starts empty");
ok($("#list").textContent.includes("Select a feed"), "and says what to do");
ok(!calls.some(([c]) => c === "news_list"), "no articles are fetched until something is picked");
ok(!doc.querySelector('.node[aria-current="true"]'), "nothing in the sidebar is highlighted");
await fire("feeds-updated", 3);
await tick(60);
ok(doc.querySelectorAll("#list .item").length === 0, "an update does not fill it either");

// Pick Unread so the rest of the suite has a list to work with.
doc.querySelector('#catlist .node[data-scope="unread"]').click();
await tick(80);
ok(doc.querySelectorAll("#list .item").length === 4, "clicking a category shows its articles");
ok(calls.some(([c, a]) => c === "news_list" && a.scope === "unread"), "for that scope");

console.log("\ndensity toggle (pure JS, no backend)");
const before = doc.documentElement.dataset.density;
click('#seg-density button[data-density="compact"]');
await tick();
ok(doc.documentElement.dataset.density === "compact", `density switched (was ${before})`);
ok($('#seg-density button[data-density="compact"]').getAttribute("aria-pressed") === "true",
   "compact button marked pressed");
click('#seg-density button[data-density="relaxed"]');
await tick();
ok(doc.documentElement.dataset.density === "relaxed", "density switched back");

console.log("\ntheme cycling");
const t0 = doc.documentElement.dataset.theme;
click("#btn-theme");
await tick();
ok(doc.documentElement.dataset.theme !== t0, `theme changed (${t0} -> ${doc.documentElement.dataset.theme})`);

console.log("\nselecting an article");
$("#list .item").click();
await tick(60);
ok($("#article .headline")?.textContent === "First post", "article headline rendered");
ok($("#article .prose")?.innerHTML.includes("Body text"), "article body rendered");
ok(calls.some(([c, a]) => c === "article" && a.id === 10), "article command invoked with the id");
ok(calls.some(([c, a]) => c === "set_read" && a.read === true), "opening marked it read");

console.log("\nlinks inside an article go to the browser");
$("#article .prose a").click();
await tick();
ok(opened.includes("https://out.test/x"), "openUrl called for an in-article link");

console.log("\nopen in browser button");
click("#openext");
await tick();
ok(opened.includes("https://kn.test/1"), "openUrl called for the current article");

console.log("\nadd feed modal (window.prompt does not exist in WebView2)");
click("#btn-addfeed");
await tick();
const modal = $(".modal-back");
ok(!!modal, "modal opened");
ok(!!$(".modal-input"), "modal has an input");
$(".modal-input").value = "https://example.test/feed.xml";
click("[data-ok]");
await tick(60);
ok(calls.some(([c, a]) => c === "add_feed" && a.url === "https://example.test/feed.xml"),
   "add_feed invoked with the typed url");
ok(calls.some(([c]) => c === "update_feed_now"), "first fetch triggered after adding");
ok(!$(".modal-back"), "modal closed");

console.log("\nadd feed from a site address");
{
  // A site with several feeds offers a choice; the one already subscribed
  // cannot be picked.
  click("#btn-addfeed"); await tick();
  $(".modal-input").value = "multi.test";
  click("[data-ok]"); await tick(60);
  ok(calls.some(([c, a]) => c === "discover_feed" && a.address === "multi.test"), "the address is looked up first");
  const choices = [...doc.querySelectorAll(".modal-back [data-choice]")];
  ok(choices.length === 2 && choices[0].disabled && !choices[1].disabled,
     "several feeds are offered, the subscribed one greyed out");
  let n = calls.length;
  choices[1].click(); await tick(60);
  ok(calls.slice(n).some(([c, a]) => c === "add_feed" && a.url === "https://multi.test/comments.xml"), "the chosen feed is added");

  click("#btn-addfeed"); await tick();
  $(".modal-input").value = "none.test";
  n = calls.length;
  click("[data-ok]"); await tick(60);
  ok($("#toast").textContent.includes("No feed found") && !calls.slice(n).some(([c]) => c === "add_feed"),
     "a site without a feed says so and adds nothing");

  click("#btn-addfeed"); await tick();
  $(".modal-input").value = "kn.test";
  n = calls.length;
  click("[data-ok]"); await tick(60);
  ok($("#toast").textContent.includes("already subscribed") && !calls.slice(n).some(([c]) => c === "add_feed"),
     "an address already subscribed says so");
}

console.log("\nadd feed when the address cannot be checked");
{
  click("#btn-addfeed"); await tick();
  $(".modal-input").value = "locked.test/rss";
  let n = calls.length;
  click("[data-ok]"); await tick(120);
  ok(calls.slice(n).some(([c, a]) => c === "add_feed" && a.url === "https://locked.test/rss"),
     "a feed that asks for a password is still added");
  ok(!!doc.querySelector(".sheet #f-user"), "and its properties open at the sign-in");
  closeSheetIfOpen();

  click("#btn-addfeed"); await tick();
  $(".modal-input").value = "down.test";
  n = calls.length;
  click("[data-ok]"); await tick(60);
  ok(doc.querySelector(".modal-back .modal-title")?.textContent.includes("Add it anyway"),
     "a server that cannot be reached asks before adding");
  click(".modal-back [data-ok]"); await tick(80);
  ok(calls.slice(n).some(([c, a]) => c === "add_feed" && a.url === "https://down.test"), "and adds it when told to");
}

console.log("\nmodal can be cancelled");
click("#btn-addfolder");
await tick();
ok(!!$(".modal-back"), "folder modal opened");
click("[data-cancel]");
await tick();
ok(!$(".modal-back"), "cancel closed it");
ok(!calls.some(([c]) => c === "add_folder"), "cancel did not call add_folder");

console.log("\nimport and export");
click("#btn-import");
await tick(60);
ok(calls.some(([c, a]) => c === "import_file" && a.path === picked.value),
   "import_file invoked with the picked path");
ok($("#btn-import").textContent.trim() === "Import",
   "button is labelled Import, not Import QuiteRSS");
click("#btn-export");
await tick(60);
ok(calls.some(([c, a]) => c === "export_opml" && a.path === saved.value),
   "export_opml invoked with the chosen path");

console.log("\nupdate all");
click("#btn-update");
await tick(60);
ok(calls.some(([c]) => c === "update_all"), "update_all invoked");

console.log("\ntoolbar actions");
click("#btn-star");
await tick();
ok(calls.some(([c]) => c === "set_starred"), "star invoked");
click("#btn-toggleread");
await tick();
ok(calls.filter(([c]) => c === "set_read").length >= 2, "toggle read invoked");
ok(!$("#btn-markall"), "the duplicate mark-all is gone from the main toolbar");
ok(!!$("#btn-listmarkall"), "mark all as read sits in the list panel");
click("#btn-listmarkall");
await tick();
ok(calls.some(([c]) => c === "mark_scope_read"), "mark all invoked");
click("#btn-delete");
await tick();
ok(calls.some(([c]) => c === "set_deleted"), "delete invoked");

console.log("\nread/unread dot");
{
  const row = doc.querySelector('.item[data-id="12"]');
  ok(!!row.querySelector("[data-dot]"), "every row has a read/unread dot");
  ok(row.querySelector(".marks .star") && row.querySelector(".marks .dot"),
     "the dot sits beside the star, before the title");
  ok(!doc.querySelector('.item[data-id="11"]').classList.contains("read") === false,
     "a read row carries the read class the hollow ring keys off");

  const before = calls.filter(([c]) => c === "set_read").length;
  const opened = calls.filter(([c]) => c === "article").length;
  row.querySelector("[data-dot]").click();
  await tick(40);
  ok(calls.filter(([c]) => c === "set_read").length > before, "clicking the dot marks it read");
  ok(calls.filter(([c]) => c === "article").length === opened,
     "clicking the dot does not open the article");
}

console.log("\nmultiple selection");
{
  const click2 = (id, mods = {}) =>
    doc.querySelector(`.item[data-id="${id}"]`)
       .dispatchEvent(new window.MouseEvent("click", { bubbles: true, ...mods }));

  click2(10);
  await tick(40);
  ok($("#selcount").classList.contains("show") === false, "one row selected shows no badge");

  click2(12, { ctrlKey: true });
  await tick();
  ok(doc.querySelector('.item[data-id="10"]').dataset.sel === "true", "first row still selected");
  ok(doc.querySelector('.item[data-id="12"]').dataset.sel === "true", "ctrl+click added a row");
  ok(doc.querySelector('.item[data-id="11"]').dataset.sel === "false", "untouched row not selected");
  ok($("#selcount").textContent === "2 selected", `badge reads the count (${$("#selcount").textContent})`);

  const opened = calls.filter(([c]) => c === "article").length;
  click2(13, { ctrlKey: true });
  await tick();
  ok(calls.filter(([c]) => c === "article").length === opened,
     "ctrl+click does not change the reading pane");
  ok($("#selcount").textContent === "3 selected", "three selected");

  // ctrl+click again removes it
  click2(13, { ctrlKey: true });
  await tick();
  ok($("#selcount").textContent === "2 selected", "ctrl+click toggles a row back off");

  // shift+click takes the range from the anchor
  click2(10);
  await tick(40);
  click2(13, { shiftKey: true });
  await tick();
  ok($("#selcount").textContent === "4 selected", "shift+click selected the whole range");
  ok([...doc.querySelectorAll(".item")].every((n) => n.dataset.sel === "true"),
     "every row in the range is marked");

  // a bulk action hits every selected id in one call
  const n = calls.length;
  click("#btn-toggleread");
  await tick(40);
  const call = calls.slice(n).find(([c]) => c === "set_read");
  ok(call && call[1].ids.length === 4, `set_read covered all 4 ids (${call?.[1].ids.length})`);

  // Escape collapses to the focused article
  doc.dispatchEvent(new window.KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  await tick();
  ok(!$("#selcount").classList.contains("show"), "Escape clears a multi-selection");

  // ctrl+A takes everything
  doc.dispatchEvent(new window.KeyboardEvent("keydown", { key: "a", ctrlKey: true, bubbles: true }));
  await tick();
  ok($("#selcount").textContent === "4 selected", "ctrl+A selects all");
  doc.dispatchEvent(new window.KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  await tick();
}

console.log("\nthe focused article can vanish from the list");
{
  // Open one, then have the list come back without it — exactly what happens
  // when a read article leaves the Unread scope on the next refresh. Every
  // toolbar action has to survive that; reaching for the missing row is what
  // threw "cannot read properties of undefined (reading 'read')".
  doc.querySelector('.item[data-id="10"]').click();
  await tick(50);
  const keep = ITEMS.splice(0, 1)[0];
  await window.eval("loadList()");
  await tick(40);
  ok(!doc.querySelector('.item[data-id="10"]'), "the row is gone from the list");

  const errors = [];
  const onErr = (e) => errors.push(e.reason?.message || e.message || String(e));
  // A rejection thrown inside window.eval'd code surfaces on the Node process,
  // not on the jsdom window, so both have to be watched or this passes blind.
  const onNodeErr = (r) => errors.push(r?.message || String(r));
  window.addEventListener("unhandledrejection", onErr);
  process.on("unhandledRejection", onNodeErr);

  click("#btn-toggleread"); await tick(40);
  click("#btn-star");       await tick(40);
  click("#btn-delete");     await tick(40);
  doc.dispatchEvent(new window.KeyboardEvent("keydown", { key: "j", bubbles: true }));
  await tick(40);

  window.removeEventListener("unhandledrejection", onErr);
  process.off("unhandledRejection", onNodeErr);
  ok(errors.length === 0, `no exception from a missing row: ${errors.join(" | ")}`);
  ITEMS.unshift(keep);
  await window.eval("loadList()");
  await tick(40);
}

console.log("\nkeyboard");
const key = (k) => doc.dispatchEvent(new window.KeyboardEvent("keydown", { key: k, bubbles: true }));
$("#list .item").click();
await tick(40);
const n0 = calls.filter(([c]) => c === "article").length;
key("j");
await tick(40);
ok(calls.filter(([c]) => c === "article").length > n0, "j moved to the next article");
key("s");
await tick();
ok(calls.filter(([c]) => c === "set_starred").length >= 2, "s starred");
{
  // Win+Shift+S is the Windows screenshot tool; it used to star the article.
  const n = calls.filter(([c]) => c === "set_starred").length;
  const before = calls.length;
  for (const mod of [{ metaKey: true, shiftKey: true }, { ctrlKey: true }, { altKey: true }, { ctrlKey: true, shiftKey: true }]) {
    doc.dispatchEvent(new window.KeyboardEvent("keydown", { key: mod.shiftKey ? "S" : "s", bubbles: true, ...mod }));
  }
  doc.dispatchEvent(new window.KeyboardEvent("keydown", { key: "m", ctrlKey: true, bubbles: true }));
  doc.dispatchEvent(new window.KeyboardEvent("keydown", { key: "Delete", ctrlKey: true, bubbles: true }));
  await tick();
  ok(calls.filter(([c]) => c === "set_starred").length === n,
     "a chord ending in S (Win+Shift+S, Ctrl+S, Alt+S) does not star");
  ok(!calls.slice(before).some(([c]) => /^(set_read|set_starred|delete)/.test(c)),
     "nor does Ctrl+M or Ctrl+Delete act on the article");
}
{
  // Ctrl+Alt+A is the WeChat/QQ screenshot key; it used to select every
  // article, so the next star went to all of them.
  const selCount = () => doc.querySelectorAll('#list .item[data-sel="true"]').length;
  $("#list .item").click();
  await tick(40);
  doc.dispatchEvent(new window.KeyboardEvent("keydown", { key: "a", ctrlKey: true, altKey: true, bubbles: true }));
  await tick();
  ok(selCount() === 1, `Ctrl+Alt+A does not select every article (${selCount()})`);
  // The reading pane's star touches the article on screen only.
  doc.dispatchEvent(new window.KeyboardEvent("keydown", { key: "a", ctrlKey: true, bubbles: true }));
  await tick();
  const all = selCount();
  const shown = Number(doc.querySelector('#list .item[aria-selected="true"]').dataset.id);
  const n = calls.length;
  $("#btn-star2").click();
  await tick();
  const c = calls.slice(n).find(([c]) => c === "set_starred");
  ok(all > 1 && c && c[1].ids.length === 1 && c[1].ids[0] === shown,
     `with ${all} selected, the reading pane star stars only the shown article (${c && c[1].ids.length})`);
  doc.dispatchEvent(new window.KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  await tick();
}
{
  // Keys pressed inside Settings used to reach the article list behind it.
  $("#list .item").click();
  await tick(40);
  await window.eval('openSettings()');
  await tick(60);
  const n = calls.length;
  for (const k of ["s", "m", "Delete", "Enter"]) {
    doc.dispatchEvent(new window.KeyboardEvent("keydown", { key: k, bubbles: true }));
  }
  const sel = doc.querySelector(".sheet select");
  if (sel) sel.dispatchEvent(new window.KeyboardEvent("keydown", { key: "s", bubbles: true }));
  await tick(40);
  ok(!calls.slice(n).some(([c]) => /^(set_starred|set_read|set_deleted|remove_feed|open)/.test(c)),
     "keys pressed while Settings is open do not act on the article behind it");
  closeSheetIfOpen();
  await tick(20);
  const m = calls.length;
  doc.dispatchEvent(new window.KeyboardEvent("keydown", { key: "s", bubbles: true }));
  await tick();
  ok(calls.slice(m).some(([c]) => c === "set_starred"), "and work again once it is closed");
  doc.dispatchEvent(new window.KeyboardEvent("keydown", { key: "s", bubbles: true }));
  await tick();
}

console.log("\nsearch filter");
$("#q").value = "Second";
$("#q").dispatchEvent(new window.Event("input"));
await tick(320);
ok(doc.querySelectorAll("#list .item").length === 1, "search narrowed the list to 1");
$("#q").value = "";
$("#q").dispatchEvent(new window.Event("input"));
await tick(320);
ok(doc.querySelectorAll("#list .item").length === 4, "clearing search restored the list");

console.log("\nbroken feed indicator");
ok($("#tree").innerHTML.includes("Last update failed"), "failing feed shows a warning");

console.log("\ncategories below the feeds");
{
  const sidebar = doc.querySelector("#feeds");
  const treeIdx = [...sidebar.children].indexOf($("#tree"));
  const catsIdx = [...sidebar.children].indexOf($("#cats"));
  ok(treeIdx >= 0 && catsIdx > treeIdx, "the categories panel sits below the feed tree");

  const cats = [...$("#catlist").querySelectorAll(".node")].map((n) => n.dataset.scope);
  ok(cats.includes("unread") && cats.includes("starred") && cats.includes("deleted"),
     "smart folders are in the categories panel");
  ok(!$("#tree").querySelector('[data-scope="unread"]'),
     "smart folders are no longer mixed into the feed tree");
  ok(cats.some((c) => c === "label:1"), "labels appear under categories");

  // collapsible, and the state is remembered
  ok($("#cats-head").getAttribute("aria-expanded") === "true", "starts expanded");
  $("#cats-head").click();
  await tick();
  ok($("#cats-head").getAttribute("aria-expanded") === "false", "collapses");
  ok(window.localStorage.getItem("catsOpen") === "false", "the collapsed state is remembered");
  $("#cats-head").click();
  await tick();
}

console.log("\napp menu");
{
  click("#btn-appmenu");
  await tick();
  const menu = doc.querySelector(".ctx");
  ok(!!menu, "the app menu opens");
  const labels = [...menu.querySelectorAll(".ctx-label")].map((n) => n.textContent);
  for (const want of ["Add", "Import…", "Export OPML…", "Create backup…",
                      "View", "Feeds", "News", "Tools", "Help", "Exit"])
    ok(labels.includes(want), `menu has "${want}"`);

  // submenus open on hover and stack
  const view = [...menu.querySelectorAll("button")]
    .find((b) => b.querySelector(".ctx-label")?.textContent === "View");
  ok(!!view.querySelector(".ctx-arrow"), "a submenu entry shows an arrow");
  view.dispatchEvent(new window.MouseEvent("mouseenter", { bubbles: true }));
  await tick();
  ok(doc.querySelectorAll(".ctx").length === 2, "the submenu opened alongside its parent");
  const subLabels = [...doc.querySelectorAll('.ctx[data-depth="1"] .ctx-label')]
    .map((n) => n.textContent);
  ok(subLabels.includes("Theme") && subLabels.includes("Density"),
     `View submenu: ${subLabels.join(", ")}`);

  doc.dispatchEvent(new window.KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  await tick();
  ok(doc.querySelectorAll(".ctx").length === 0, "Escape closes the whole stack");
}

console.log("\nsettings window");
{
  doc.dispatchEvent(new window.KeyboardEvent("keydown", { key: ",", ctrlKey: true, bubbles: true }));
  await tick(60);
  ok(!!doc.querySelector(".sheet"), "Ctrl+, opens settings");
  ok(calls.some(([c]) => c === "get_settings"), "settings were loaded");

  const pages = [...doc.querySelectorAll("[data-page]")].map((b) => b.dataset.page);
  ok(pages.join(",") === "general,appearance,toolbars,reading,labels,filters,cleanup,shortcuts,updates,about",
     `all pages present (${pages.join(",")})`);

  // the cleanup page pulls live database stats
  doc.querySelector('[data-page="cleanup"]').click();
  await tick(60);
  ok(calls.some(([c]) => c === "db_stats"), "the storage page reads db_stats");
  ok(doc.querySelector(".sheet-page").textContent.includes("2.0 MB"), "file size shown");

  // a switch edits the pending set, and saving sends it
  const swUnread = doc.querySelector('[data-sw="cleanup.never_delete_unread"]');
  ok(!!swUnread, "the unread protection has a switch");
  ok(swUnread.getAttribute("aria-checked") === "true", "it reflects the stored value");
  swUnread.click();
  await tick();
  ok(swUnread.getAttribute("aria-checked") === "false", "it toggles");

  const n = calls.length;
  doc.querySelector("[data-save]").click();
  await tick(60);
  const saveCall = calls.slice(n).find(([c]) => c === "set_settings");
  ok(!!saveCall, "Save writes the settings");
  ok(saveCall[1].values["cleanup.never_delete_unread"] === "0",
     "the toggled value is what gets written");
  ok(!doc.querySelector(".sheet"), "the window closes after saving");
}

console.log("\nstorage actions");
{
  await window.eval('openSettings("cleanup")');
  await tick(80);
  const run = (act) => doc.querySelector(`[data-act="${act}"]`).click();

  run("cleanup"); await tick(60);
  ok(calls.some(([c]) => c === "run_cleanup"), "clean up now runs the retention rules");
  // Settings are saved first, or cleanup runs against the values on disk and
  // looks like it ignored what was just typed.
  const idx = calls.map(([c]) => c);
  ok(idx.lastIndexOf("set_settings") < idx.lastIndexOf("run_cleanup"),
     "settings are saved before cleanup runs");

  await window.eval('openSettings("cleanup")');
  await tick(80);
  run("clearcache"); await tick(60);
  ok(calls.some(([c]) => c === "clear_article_cache"), "clear cache invoked");

  await window.eval('openSettings("cleanup")');
  await tick(80);
  run("vacuum"); await tick(60);
  ok(calls.some(([c]) => c === "vacuum_db"), "compact invoked");

  await window.eval('openSettings("cleanup")');
  await tick(80);
  run("backup"); await tick(60);
  ok(calls.some(([c, a]) => c === "backup_db" && a.path === saved.value), "backup invoked");

  closeSheetIfOpen();
}

console.log("\nfeed properties");
{
  await window.eval("openFeedSettings(2)");
  await tick(80);
  ok(!!doc.querySelector(".sheet"), "feed properties opens");
  ok(calls.some(([c, a]) => c === "feed_settings" && a.id === 2), "it loads that feed");
  ok(doc.querySelector("#f-name")?.value === "Kernel Notes", "the name is filled in");
  ok(!!doc.querySelector('[data-sw="maxToKeep"], [data-sw="keepEnable"]'),
     "per-feed retention is editable");

  doc.querySelector('[data-sw="keepEnable"]').click();
  await tick();
  const n = calls.length;
  doc.querySelector("[data-save]").click();
  await tick(60);
  const saved2 = calls.slice(n).find(([c]) => c === "save_feed_settings");
  ok(!!saved2, "saving writes the feed settings");
  ok(saved2[1].s.maxToKeepEnable === true, "the toggled per-feed rule is sent");
  ok(saved2[1].s.signInUser === "" && saved2[1].s.signInPassword === null,
     "no sign-in typed: none is sent, and no password");
  closeSheetIfOpen();

  // Sign-in: a user name and password go to the backend; a blank password
  // later keeps the saved one.
  feedSignIn.value = { signInUser: "ann", hasPassword: true };
  await window.eval("openFeedSettings(2)");
  await tick(80);
  ok(doc.querySelector("#f-user").value === "ann", "the saved user name is shown");
  ok(doc.querySelector("#f-pass").value === "" && doc.querySelector("#f-pass").placeholder.includes("Saved"),
     "the saved password is not sent to the page, only that there is one");
  let m = calls.length;
  doc.querySelector("[data-save]").click();
  await tick(60);
  let sv = calls.slice(m).find(([c]) => c === "save_feed_settings");
  ok(sv[1].s.signInUser === "ann" && sv[1].s.signInPassword === null, "a blank password keeps the saved one");
  closeSheetIfOpen();
  await window.eval("openFeedSettings(2)");
  await tick(80);
  doc.querySelector("#f-pass").value = "s3cret";
  m = calls.length;
  doc.querySelector("[data-save]").click();
  await tick(60);
  sv = calls.slice(m).find(([c]) => c === "save_feed_settings");
  ok(sv[1].s.signInPassword === "s3cret", "a typed password is saved");
  feedSignIn.value = {};
  closeSheetIfOpen();
}

// ---------------------------------------------------------------- labels UI
console.log("\nlabels");
{
  // Chips on the rows, drawn from the label list rather than from the article.
  const chips = [...doc.querySelectorAll('.item[data-id="10"] .lchip')];
  ok(chips.length === 1, `the first row shows its one label (${chips.length})`);
  ok(chips[0].textContent === "Important", "with the label's name");
  ok(doc.querySelectorAll('.item[data-id="11"] .lchip').length === 2,
     "a row with two labels shows two chips");
  ok(doc.querySelectorAll('.item[data-id="12"] .lchip').length === 0,
     "a row with none shows none");

  // The label menu ticks what the selection already has.
  doc.querySelector('.item[data-id="10"]').click();
  await tick(60);
  doc.querySelector("#btn-labelmenu").click();
  await tick();
  const menu = doc.querySelector(".ctx");
  ok(!!menu, "the labels button opens a menu");
  const rows = [...menu.querySelectorAll("button")];
  const important = rows.find((b) => b.textContent.includes("Important"));
  const later = rows.find((b) => b.textContent.includes("Read later"));
  ok(!!important && !!later, "every label is offered");
  ok(important.querySelector(".tick svg"), "a label the article has is ticked");
  ok(!later.querySelector(".tick svg"), "one it does not have is not");

  // Clicking a ticked label takes it off; clicking an unticked one puts it on.
  let n = calls.length;
  later.click();
  await tick(60);
  let call = calls.slice(n).find(([c]) => c === "set_label");
  ok(!!call, "choosing a label writes it");
  ok(call[1].labelId === 2 && call[1].on === true, "an unticked label is turned on");
  ok(Array.isArray(call[1].ids) && call[1].ids.includes(10), "for the selected article");

  doc.querySelector("#btn-labelmenu").click();
  await tick();
  n = calls.length;
  [...doc.querySelectorAll(".ctx button")]
    .find((b) => b.textContent.includes("Important")).click();
  await tick(60);
  call = calls.slice(n).find(([c]) => c === "set_label");
  ok(call && call[1].on === false, "a ticked label is turned off");

  // Managing them.
  await window.eval('openSettings("labels")');
  await tick(80);
  ok(doc.querySelectorAll("[data-ledit]").length === 2, "both labels are listed");
  ok(doc.querySelector(".sheet-page").textContent.includes("3 articles"),
     "with how many articles carry each");

  n = calls.length;
  doc.querySelector('[data-lmove="1:1"]').click();
  await tick(60);
  ok(calls.slice(n).some(([c, a]) => c === "reorder_label" && a.id === 1 && a.delta === 1),
     "reordering is sent");

  await window.eval('openSettings("labels")');
  await tick(80);
  doc.querySelector("[data-lnew]").click();
  await tick(40);
  const dlg = doc.querySelector(".modal [data-name]");
  ok(!!dlg, "New label opens an editor");
  dlg.value = "Urgent";
  doc.querySelectorAll(".modal [data-c]")[2].click();
  n = calls.length;
  doc.querySelector(".modal [data-ok]").click();
  await tick(80);
  const made = calls.slice(n).find(([c]) => c === "save_label");
  ok(!!made, "creating a label writes it");
  ok(made[1].draft.name === "Urgent", "with the typed name");
  ok(/^#[0-9a-f]{6}$/i.test(made[1].draft.colorBg), "and the chosen colour");
  ok(made[1].draft.id === null, "as a new row rather than an edit");
  closeSheetIfOpen();
}

// --------------------------------------------------------------- filters UI
console.log("\nfilters");
{
  await window.eval('openSettings("filters")');
  await tick(80);
  const page = doc.querySelector(".sheet-page");
  ok(calls.some(([c]) => c === "filters"), "the filters page loads them");
  ok(page.textContent.includes("Drop sponsored"), "a filter is listed");
  ok(page.textContent.includes("all conditions"), "with how its conditions combine");
  ok(doc.querySelector('[data-fen="2"]').getAttribute("aria-checked") === "false",
     "a disabled filter shows as off");

  let n = calls.length;
  doc.querySelector('[data-fen="2"]').click();
  await tick(60);
  ok(calls.slice(n).some(([c, a]) => c === "set_filter_enabled" && a.id === 2 && a.on === true),
     "the switch enables it");

  await window.eval('openSettings("filters")');
  await tick(80);
  n = calls.length;
  doc.querySelector('[data-fmove="1:1"]').click();
  await tick(60);
  ok(calls.slice(n).some(([c, a]) => c === "reorder_filter" && a.delta === 1),
     "order can be changed, because order decides what sees what");

  // The editor.
  await window.eval('openSettings("filters")');
  await tick(80);
  doc.querySelector('[data-fedit="1"]').click();
  await tick(100);
  ok(!!doc.querySelector(".modal [data-name]"), "editing opens the filter editor");
  ok(doc.querySelector(".modal [data-name]").value === "Drop sponsored", "loaded with its name");
  ok(doc.querySelector('.modal [data-cv="0"]').value === "sponsored", "and its condition");

  // The operator list has to follow the field, or a saved filter means
  // something different from what it shows.
  const fieldSel = doc.querySelector('.modal [data-cf="0"]');
  ok([...doc.querySelectorAll('.modal [data-co="0"] option')].length === 7,
     "title offers seven operators");
  fieldSel.value = "description";
  fieldSel.dispatchEvent(new window.Event("change"));
  await tick(40);
  ok([...doc.querySelectorAll('.modal [data-co="0"] option')].length === 3,
     "switching to the body narrows them to three");
  ok(doc.querySelector('.modal [data-co="0"]').value === "contains",
     "and the operator falls back to a legal one");

  // Status swaps the free-text box for a fixed list.
  fieldSel.value = "status";
  fieldSel.dispatchEvent(new window.Event("change"));
  await tick(40);
  ok(doc.querySelector('.modal [data-cv="0"]').tagName === "SELECT",
     "status is chosen from a list, not typed");

  fieldSel.value = "title";
  fieldSel.dispatchEvent(new window.Event("change"));
  await tick(40);
  doc.querySelector('.modal [data-cv="0"]').value = "promoted";
  doc.querySelector('.modal [data-cv="0"]').dispatchEvent(new window.Event("input"));

  doc.querySelector(".modal [data-cadd]").click();
  await tick(40);
  ok(doc.querySelectorAll(".modal [data-cf]").length === 2, "a condition can be added");

  n = calls.length;
  doc.querySelector(".modal [data-ok]").click();
  await tick(100);
  const saved3 = calls.slice(n).find(([c]) => c === "save_filter");
  ok(!!saved3, "saving writes the filter");
  ok(saved3[1].draft.id === 1, "as an edit of the same filter");
  ok(saved3[1].draft.conditions[0].content === "promoted", "carrying the edited text");
  ok(saved3[1].draft.conditions.length === 2, "and both conditions");

  // Every-article mode has no conditions to send.
  await window.eval('openSettings("filters")');
  await tick(80);
  doc.querySelector("[data-fnew]").click();
  await tick(100);
  doc.querySelector(".modal [data-name]").value = "Mark everything";
  doc.querySelector(".modal [data-name]").dispatchEvent(new window.Event("input"));
  const mode = doc.querySelector(".modal [data-mode]");
  mode.value = "0";
  mode.dispatchEvent(new window.Event("change"));
  await tick(40);
  ok(!doc.querySelector(".modal [data-cf]"), "every-article mode hides the conditions");
  n = calls.length;
  doc.querySelector(".modal [data-ok]").click();
  await tick(100);
  const made2 = calls.slice(n).find(([c]) => c === "save_filter");
  ok(made2 && made2[1].draft.mode === 0 && made2[1].draft.conditions.length === 0,
     "and sends none");
  ok(made2[1].draft.id === null, "as a new filter");

  closeSheetIfOpen();
}

// ------------------------------------------------------- drag and drop
console.log("\ndrag and drop in the tree");
{
  const drag = (el, type, extra = {}) => {
    const e = new window.Event(type, { bubbles: true, cancelable: true });
    e.dataTransfer = { setData() {}, effectAllowed: "", dropEffect: "" };
    Object.assign(e, extra);
    el.dispatchEvent(e);
    return e;
  };
  const nodeFor = (id) => doc.querySelector(`#tree .node[data-id="${id}"]`);
  const rect = (el, top, height) => {
    el.getBoundingClientRect = () => ({ top, height, left: 0, right: 200, bottom: top + height, width: 200 });
  };

  const folder = nodeFor(1);
  const feed2 = nodeFor(2);
  const feed3 = nodeFor(3);
  ok(!!folder && !!feed2, "tree nodes exist");
  ok(feed2.draggable === true, "feeds are draggable");
  ok(folder.draggable === true, "so are folders");
  // Chromium refuses to start a drag from a form control, so a tree row built
  // as <button> is undraggable in WebView2 however draggable is set. It works
  // in WebKitGTK, which is why this only showed up on Windows.
  ok(feed2.tagName === "DIV", `a tree row is not a form control (${feed2.tagName})`);
  ok(feed2.getAttribute("role") === "button", "but still announces itself as a button");
  ok(feed2.tabIndex === 0, "and is reachable from the keyboard");
  ok(doc.querySelector('#catlist .node')?.draggable !== true,
     "but a smart category is not a place, so it does not drag");

  // Into a folder: the middle of a folder row.
  rect(folder, 0, 30);
  drag(feed2, "dragstart");
  drag(folder, "dragover", { clientY: 15 });
  ok(folder.classList.contains("drop-into"), "the middle of a folder means 'put it inside'");

  let n = calls.length;
  drag(folder, "drop", { clientY: 15 });
  await tick(60);
  let mv = calls.slice(n).find(([c]) => c === "move_node");
  ok(!!mv, "dropping moves the node");
  ok(mv[1].id === 2 && mv[1].target === 1 && mv[1].whereTo === "into",
     `into the folder (${JSON.stringify(mv?.[1])})`);

  // Top of a folder row means beside it, not inside it.
  drag(feed2, "dragstart");
  drag(folder, "dragover", { clientY: 2 });
  ok(folder.classList.contains("drop-before"), "the top edge means 'put it above'");
  n = calls.length;
  drag(folder, "drop", { clientY: 2 });
  await tick(60);
  mv = calls.slice(n).find(([c]) => c === "move_node");
  ok(mv && mv[1].whereTo === "before", "and that is what is sent");

  // A feed has no inside, so its row splits in half.
  rect(feed3, 0, 30);
  drag(feed2, "dragstart");
  drag(feed3, "dragover", { clientY: 25 });
  ok(feed3.classList.contains("drop-after"), "the lower half of a feed means 'put it below'");
  ok(!feed3.classList.contains("drop-into"), "never inside, because a feed holds nothing");

  // Dropping on yourself does nothing at all.
  drag(feed2, "dragstart");
  n = calls.length;
  drag(feed2, "drop", { clientY: 15 });
  await tick(40);
  ok(!calls.slice(n).some(([c]) => c === "move_node"), "dropping a node on itself is ignored");

  // Empty space under the tree is the root, which is how a feed leaves a folder.
  drag(feed2, "dragstart");
  n = calls.length;
  const rootDrop = new window.Event("drop", { bubbles: false, cancelable: true });
  rootDrop.dataTransfer = { setData() {} };
  Object.defineProperty(rootDrop, "target", { value: doc.querySelector("#tree") });
  doc.querySelector("#tree").dispatchEvent(rootDrop);
  await tick(60);
  mv = calls.slice(n).find(([c]) => c === "move_node");
  ok(mv && mv[1].target === null, "dropping on empty space moves it to the root");
}

// --------------------------------------------------------- newspaper layout
console.log("\nnewspaper layout");
{
  await window.eval('setLayout("newspaper")');
  await tick(80);
  ok(doc.documentElement.dataset.layout === "newspaper", "the layout switches");
  ok(window.localStorage.getItem("layout") === "newspaper", "and is remembered");

  const card = doc.querySelector('.item[data-id="12"]');
  ok(!!card.querySelector(".ex"), "an unopened card shows its excerpt");
  ok(card.querySelector(".ex").textContent.includes("Third excerpt"),
     "the excerpt comes from the backend, not from HTML in the browser");
  ok(!!card.querySelector(".head"), "and keeps its marks and meta");

  card.click();
  await tick(100);
  const open = doc.querySelector('.item[data-id="12"]');
  ok(!!open.querySelector(".full .prose"), "clicking expands the article in the card");
  ok(open.querySelector(".full .prose").innerHTML.includes("Body text"),
     "with the sanitised body");
  ok(!!open.querySelector("[data-collapse]"), "and a way to collapse it again");
  ok(!open.querySelector(".ex"), "the excerpt is replaced, not duplicated");

  // Other cards stay collapsed: one article open at a time.
  ok(doc.querySelectorAll(".full .prose").length === 1, "only one card is open");

  open.querySelector("[data-collapse]").click();
  await tick(60);
  ok(!doc.querySelector(".full .prose"), "collapse closes it");

  // Clicking the headline of the open card closes it too.
  doc.querySelector('.item[data-id="12"]').click();
  await tick(100);
  ok(!!doc.querySelector('.item[data-id="12"] .full .prose'), "clicking the card again reopens it");
  doc.querySelector('.item[data-id="12"] .head .t').click();
  await tick(60);
  ok(!doc.querySelector(".full .prose"), "clicking the open card's title closes it");
  // The second click of a double-click does not close it.
  doc.querySelector('.item[data-id="12"]').click();
  await tick(100);
  doc.querySelector('.item[data-id="12"] .head .t')
    .dispatchEvent(new window.MouseEvent("click", { bubbles: true, detail: 2 }));
  await tick(60);
  ok(!!doc.querySelector('.item[data-id="12"] .full .prose'), "but a double-click's second click leaves it open");
  doc.querySelector('.item[data-id="12"] [data-collapse]').click();
  await tick(60);

  // A click on a link inside an open card must not also re-open the card.
  doc.querySelector('.item[data-id="12"]').click();
  await tick(100);
  const link = doc.querySelector(".full .prose a[href]");
  ok(!!link, "links survive into the card");
  const before = calls.filter(([c]) => c === "article").length;
  link.click();
  await tick(40);
  ok(calls.filter(([c]) => c === "article").length === before,
     "clicking a link in the body does not reload the article");
  ok(opened.includes("https://out.test/x"), "it opens in the real browser instead");

  await window.eval('setLayout("classic")');
  await tick(100);
  ok(doc.documentElement.dataset.layout === "classic", "and back again");
  ok(!doc.querySelector(".full .prose"), "the cards are gone");
}

// ------------------------------------------------------ customisable toolbars
console.log("\ntoolbars");
{
  const bar = doc.querySelector("#toolbar");
  const cmds = () => [...bar.querySelectorAll("[data-cmd]")]
    .filter((b) => !b.hidden).map((b) => b.dataset.cmd);

  ok(cmds().includes("update"), "the main bar has its buttons");
  ok(!cmds().includes("markall"),
     "and not the ones left out of the default layout");
  ok(cmds().includes("settings"), "Settings has a button by default, not only a menu entry");

  await window.eval('openSettings("toolbars")');
  await tick(80);
  ok(doc.querySelectorAll("[data-tbshow]").length === 4,
     "all four toolbars are listed");

  // Adding a button.
  const add = doc.querySelector('[data-tbadd="main"]');
  add.value = "markall";
  add.dispatchEvent(new window.Event("change"));
  await tick(60);
  ok(cmds().includes("markall"), "adding a button puts it on the bar straight away");
  ok(JSON.parse(window.localStorage.getItem("toolbars")).main.items.includes("markall"),
     "and remembers it");

  // The handler bound at startup has to survive being moved. This is the whole
  // reason customisation reorders live elements instead of rebuilding them.
  let n = calls.length;
  doc.querySelector("#btn-update").click();
  await tick(60);
  ok(calls.slice(n).some(([c]) => c === "update_all"),
     "a button that has been moved still works");

  // Removing one.
  await window.eval('openSettings("toolbars")');
  await tick(80);
  const items = JSON.parse(window.localStorage.getItem("toolbars")).main.items;
  const at = items.indexOf("theme");
  doc.querySelector(`[data-tbdrop="main:${at}"]`).click();
  await tick(60);
  ok(!cmds().includes("theme"), "removing a button hides it");
  ok(doc.querySelector("#btn-theme"), "without destroying it");

  // Reordering.
  await window.eval('openSettings("toolbars")');
  await tick(80);
  const before2 = cmds();
  doc.querySelector('[data-tbmove="main:2:-1"]').click();
  await tick(60);
  ok(cmds().join(",") !== before2.join(","), "moving a button changes the order on the bar");

  // Button style.
  await window.eval('openSettings("toolbars")');
  await tick(80);
  const style = doc.querySelector('[data-tbstyle="news"]');
  style.value = "text";
  style.dispatchEvent(new window.Event("change"));
  await tick(60);
  ok(doc.querySelector("#listbar").dataset.style === "text",
     "the button style applies to that bar only");
  ok(doc.querySelector("#toolbar").dataset.style !== "text", "not to the others");

  // Hiding a whole bar.
  await window.eval('openSettings("toolbars")');
  await tick(80);
  doc.querySelector('[data-tbshow="feeds"]').click();
  await tick(60);
  ok(doc.querySelector("#feedsbar").hidden === true, "a toolbar can be hidden");

  // Reset.
  await window.eval('openSettings("toolbars")');
  await tick(80);
  doc.querySelector('[data-tbreset="main"]').click();
  await tick(60);
  ok(!cmds().includes("markall") && cmds().includes("theme"),
     "reset puts the default layout back");

  closeSheetIfOpen();
  // Leave the app as the next run expects to find it.
  window.localStorage.removeItem("toolbars");
  window.localStorage.removeItem("layout");
}

function closeSheetIfOpen() {
  doc.querySelector(".sheet-back")?.remove();
  doc.querySelector(".modal-back")?.remove();
}

// --------------------------------------------------- colours survive the CSP
console.log("\ncolours are applied through the CSSOM");
{
  // Tauri puts a nonce on style-src, and a nonce makes 'unsafe-inline' inert
  // for style attributes as well as <style> elements. Every runtime colour
  // this app wrote as style="background:…" was therefore dropped by the real
  // webview while looking perfectly fine in this harness. Colours now ride in
  // on data-bg and are set from script, which CSP does not govern.
  const chip = doc.querySelector(".item .lchip");
  ok(!!chip, "a label chip is drawn");
  ok(!chip.getAttribute("style")?.includes("background") ||
     chip.dataset.bg !== undefined,
     "the chip carries its colour as data, not as a style attribute");
  ok(chip.style.background !== "", `the colour was applied (${chip.style.background})`);

  const avatar = doc.querySelector("span.favicon");
  ok(avatar.dataset.bg?.startsWith("hsl("), "feed avatars are tinted the same way");
  ok(avatar.style.background !== "", "and that tint is applied");
  // Space-separated hsl() is CSS Color 4 and older WebKitGTK drops the whole
  // declaration, which left every avatar with no background at all.
  ok(avatar.dataset.bg.includes(","), `hsl() uses commas (${avatar.dataset.bg})`);

  // And statically: no template in app.js may write a colour into a style
  // attribute. paint() legitimately sets el.style afterwards, so the live DOM
  // cannot tell the two apart — the authored source can.
  const authored = [...js.matchAll(/style="([^"]*)"/g)]
    .map((m) => m[1])
    .filter((v) => /background\s*:|(^|;)\s*color\s*:/.test(v));
  ok(authored.length === 0,
     `no colour is authored into a style attribute (${authored.join(" | ")})`);
}

// ------------------------------------------------ things reported from use
console.log("\nreported problems");
{
  // "Text only" made the hamburger vanish, and with it the only route to
  // Settings once the toolbars were customised away.
  await window.eval('saveToolbars({ ...toolbarConfig(), main: { show: true, style: "text", items: ["appmenu", "update"] } })');
  await tick(60);
  const menuBtn = doc.querySelector("#btn-appmenu");
  ok(!menuBtn.hidden, "the app menu survives text-only mode");
  ok(!!menuBtn.querySelector(".tlabel"), "because it has a text label of its own");

  // Hiding every toolbar must not lock Settings away.
  await window.eval('saveToolbars({ ...toolbarConfig(), main: { show: false, style: "icon", items: [] } })');
  await tick(60);
  ok(doc.querySelector("#toolbar").hidden === false,
     "the main bar comes back when the menu has nowhere else to live");
  ok(!doc.querySelector("#btn-appmenu").hidden, "with the menu on it");

  // ...and the background context menu carries the whole menu as well.
  doc.querySelector("#status").dispatchEvent(
    new window.MouseEvent("contextmenu", { bubbles: true, cancelable: true }));
  await tick(40);
  const bg = [...doc.querySelectorAll(".ctx button")].map((b) => b.textContent);
  ok(bg.some((t) => t.includes("Settings")), `right-click reaches Settings (${bg.join("|")})`);
  await window.eval("closeCtx()");

  // "Mark all read" had no way onto the Feeds bar.
  await window.eval('localStorage.removeItem("toolbars"); applyToolbars();');
  await window.eval('openSettings("toolbars")');
  await tick(80);
  const add = doc.querySelector('[data-tbadd="feeds"]');
  const offered = [...add.options].map((o) => o.value);
  ok(offered.includes("markall"), `any command can go on any bar (${offered.join(",")})`);
  add.value = "markall";
  add.dispatchEvent(new window.Event("change"));
  await tick(60);
  const onFeeds = doc.querySelector('#feedsbar [data-cmd="markall"]');
  ok(onFeeds && !onFeeds.hidden, "and it appears there");

  // The clone forwards to the one real handler.
  closeSheetIfOpen();
  await window.eval('selectScope("unread", "Unread")');
  await tick(60);
  let n = calls.length;
  onFeeds.click();
  await tick(60);
  ok(calls.slice(n).some(([c]) => c === "mark_scope_read"),
     "a cloned button runs the same action as the original");

  // The app menu itself cannot be removed from the main bar.
  await window.eval('openSettings("toolbars")');
  await tick(80);
  const lock = doc.querySelector('[data-tbdrop="main:0"]');
  ok(lock.disabled, "the menu cannot be removed from the main toolbar");
  closeSheetIfOpen();
  window.localStorage.removeItem("toolbars");
  await window.eval("applyToolbars()");
}

console.log("\nundo");
{
  await window.eval('selectScope("unread", "Unread")');
  await tick(60);
  doc.querySelector('.item[data-id="10"]').click();
  await tick(60);
  let n = calls.length;
  doc.querySelector("#btn-delete").click();
  await tick(80);
  const del = calls.slice(n).find(([c]) => c === "set_deleted");
  ok(del && del[1].deleted === true, "Delete soft-deletes");

  n = calls.length;
  await window.eval("undo()");
  await tick(80);
  const back = calls.slice(n).find(([c]) => c === "set_deleted");
  ok(back && back[1].deleted === false, "Ctrl+Z puts it back");
  ok(back[1].ids.includes(10), "the same article");

  // Mark-all-read is reversible too.
  n = calls.length;
  doc.querySelector("#btn-listmarkall").click();
  await tick(80);
  n = calls.length;
  await window.eval("undo()");
  await tick(80);
  const unread = calls.slice(n).find(([c]) => c === "set_read");
  ok(unread && unread[1].read === false, "so is marking a view read");

}

console.log("\nupdate progress");
{
  await fire("update-progress", { done: 1, total: 4, title: "Kernel Notes", ok: true });
  await tick(40);
  ok(doc.querySelector("#progress").classList.contains("show"), "a progress bar appears");
  ok(doc.querySelector("#st-msg").textContent.includes("1/4"),
     `the status line counts feeds (${doc.querySelector("#st-msg").textContent})`);
  ok(doc.querySelector("#st-msg").textContent.includes("Kernel Notes"),
     "and names the one being fetched");
  ok(doc.querySelector("#btn-update").querySelector("svg").classList.contains("spin"),
     "the button spins");

  await fire("update-progress", { done: 4, total: 4, title: "", ok: true });
  await tick(40);
  ok(!doc.querySelector("#progress").classList.contains("show"), "and clears when done");
}

console.log("\nthe article upgrades itself");
{
  doc.querySelector('.item[data-id="10"]').click();
  await tick(80);
  const n = calls.length;
  // A late arrival for an article the user has moved on from must be ignored.
  await fire("article-ready", { id: 999, ok: true });
  await tick(40);
  ok(!calls.slice(n).some(([c, a]) => c === "article" && a.id === 999),
     "a result for another article is ignored");

  await fire("article-ready", { id: state_selected(), ok: true });
  await tick(60);
  ok(calls.slice(n).some(([c]) => c === "article"),
     "the one being read is re-fetched and swapped in");
}
function state_selected() {
  return Number(doc.querySelector('.item[aria-selected="true"]').dataset.id);
}

console.log("\nresizable panels");
{
  const grip = doc.querySelector("#cats-grip");
  ok(!!grip, "the categories panel has a grip");
  ok(!!doc.querySelector("#grip-sidebar") && !!doc.querySelector("#grip-list"),
     "so do the two vertical dividers");

  const before = doc.documentElement.style.getPropertyValue("--h-cats");
  const down = new window.Event("pointerdown", { bubbles: true, cancelable: true });
  Object.assign(down, { clientX: 0, clientY: 500, pointerId: 1 });
  grip.setPointerCapture = () => {};
  grip.releasePointerCapture = () => {};
  grip.dispatchEvent(down);
  const move = new window.Event("pointermove", { bubbles: true });
  Object.assign(move, { clientX: 0, clientY: 420, pointerId: 1 });
  grip.dispatchEvent(move);
  const after = doc.documentElement.style.getPropertyValue("--h-cats");
  ok(after && after !== before, `dragging it resizes the panel (${before} -> ${after})`);
  grip.dispatchEvent(new window.Event("pointerup", { bubbles: true }));
  await tick(30);
  ok(window.localStorage.getItem("h-cats") === after, "and the size is remembered");

  grip.dispatchEvent(new window.Event("dblclick", { bubbles: true }));
  ok(!window.localStorage.getItem("h-cats"), "double-click resets it");
}

console.log("\ndouble-click in the tree");
{
  const feed = doc.querySelector('#tree .node[data-id="2"]');
  const n = calls.length;
  feed.dispatchEvent(new window.MouseEvent("dblclick", { bubbles: true, cancelable: true }));
  await tick(80);
  ok(calls.slice(n).some(([c, a]) => c === "feed_settings" && a.id === 2),
     "double-clicking a feed opens its properties");
  closeSheetIfOpen();
}

console.log("\nstartup and tray");
{
  await window.eval('openSettings("general")');
  await tick(80);
  const sw2 = doc.querySelector('[data-sw="_autostart"]');
  ok(!!sw2, "there is a start-at-login switch");
  ok(!!doc.querySelector('[data-sw="startup.minimized"]'), "and start minimised");
  ok(!!doc.querySelector('[data-sw="startup.close_to_tray"]'), "and close to tray");
  ok(!!doc.querySelector('[data-sw="startup.minimize_to_tray"]'), "and minimise to tray");

  let n = calls.length;
  sw2.click();
  await tick(80);
  const set = calls.slice(n).find(([c]) => c === "set_autostart");
  ok(set && set[1].on === true, "toggling it writes to the OS immediately");
  ok(!calls.slice(n).some(([c]) => c === "set_settings"),
     "the login item is not a database setting, so Save is not involved");

  n = calls.length;
  doc.querySelector("[data-save]").click();
  await tick(80);
  const saved3 = calls.slice(n).find(([c]) => c === "set_settings");
  ok(saved3 && saved3[1].values._autostart === undefined,
     "and it is stripped before the settings are written");
  ok(calls.slice(n).some(([c]) => c === "set_close_to_tray"),
     "the close handler is told about the tray setting");
  closeSheetIfOpen();
}

console.log("\ncollapsible folders");
{
  const kids = () => doc.querySelectorAll('#tree .node[data-id="2"], #tree .node[data-id="3"]').length;
  const chev = () => doc.querySelector('#tree .node[data-id="1"] [data-toggle]');
  ok(kids() === 2, "an expanded folder shows its feeds");
  ok(chev()?.getAttribute("aria-expanded") === "true", "and its chevron says so");

  let n = calls.length;
  chev().click();
  await tick(80);
  const set = calls.slice(n).find(([c]) => c === "set_expanded");
  ok(set && set[1].id === 1 && set[1].expanded === false, "the chevron collapses it and saves that");
  ok(kids() === 0, "its feeds are hidden");
  ok(chev().getAttribute("aria-expanded") === "false", "and the chevron turns");
  ok(!calls.slice(n).some(([c, a]) => c === "news_list" && a.scope === "folder:1"),
     "folding a folder does not also open it");

  // Keyboard: Right unfolds, Left folds.
  const folder = doc.querySelector('#tree .node[data-id="1"]');
  folder.dispatchEvent(new window.KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }));
  await tick(80);
  ok(kids() === 2, "Right arrow unfolds it");
  doc.querySelector('#tree .node[data-id="1"]').dispatchEvent(
    new window.KeyboardEvent("keydown", { key: "ArrowLeft", bubbles: true }));
  await tick(80);
  ok(kids() === 0, "Left arrow folds it");

  // Hovering a collapsed folder mid-drag springs it open.
  const src = doc.querySelector('#tree .node:not([data-id="1"])') || doc.querySelector('#catlist .node');
  const target = doc.querySelector('#tree .node[data-id="1"]');
  target.getBoundingClientRect = () => ({ top: 0, height: 30, left: 0, right: 200, bottom: 30, width: 200 });
  // Start a drag from the folder itself is not allowed; fake one via a feed that
  // exists while collapsed: re-expand, grab a feed, collapse, hover.
  await window.eval("toggleFolder({ id: 1, expanded: false }, true)");
  await tick(80);
  const feed = doc.querySelector('#tree .node[data-id="2"]');
  const ds = new window.Event("dragstart", { bubbles: true });
  ds.dataTransfer = { setData() {}, effectAllowed: "" };
  feed.dispatchEvent(ds);
  await window.eval("toggleFolder({ id: 1, expanded: true }, false)");
  await tick(80);
  const t2 = doc.querySelector('#tree .node[data-id="1"]');
  t2.getBoundingClientRect = () => ({ top: 0, height: 30, left: 0, right: 200, bottom: 30, width: 200 });
  const over = new window.Event("dragover", { bubbles: true, cancelable: true });
  Object.assign(over, { clientY: 15, dataTransfer: { dropEffect: "" } });
  t2.dispatchEvent(over);
  await tick(900);
  ok(kids() === 2, "hovering a collapsed folder while dragging opens it");
  const de = new window.Event("dragend", { bubbles: true });
  (doc.querySelector('#tree .node[data-id="2"]') || feed).dispatchEvent(de);
}

console.log("\nstartup cost");
{
  // Excerpts mean reading every article body. Only the newspaper layout shows
  // them, so the classic list must not ask for them.
  const lists = calls.filter(([c]) => c === "news_list");
  const first = lists[0];
  ok(first && first[1].excerpts === false,
     `the classic list does not ask for excerpts (${JSON.stringify(first?.[1])})`);
  ok(lists.some(([, a]) => a.excerpts === true),
     "the newspaper layout does");
  // The backend reads close-to-tray from the database at launch; the frontend
  // asking again was a wasted round trip before the first paint.
  const startCalls = calls.slice(0, calls.findIndex(([c]) => c === "feed_tree") + 1).map(([c]) => c);
  ok(!startCalls.includes("set_close_to_tray"),
     `nothing waits on tray plumbing before the tree loads (${startCalls.join(",")})`);
  ok(!startCalls.includes("get_settings"), "or on the settings");
}

console.log("\ndialogs from Settings");
{
  // Stacking: a dialog must sit above the Settings sheet, or it opens behind
  // it where it cannot be seen and every further click adds another.
  const css = html.match(/<style>([\s\S]*?)<\/style>/)[1];
  const zOf = (sel) => {
    const m = css.match(new RegExp(sel.replace(".", "\\.") + "\\s*\\{[^}]*?z-index:\\s*(\\d+)"));
    return m ? Number(m[1]) : NaN;
  };
  const zModal = zOf(".modal-back"), zSheet = zOf(".sheet-back"), zCtx = zOf(".ctx");
  ok(zModal > zSheet, `dialogs sit above the Settings sheet (${zModal} > ${zSheet})`);
  ok(zModal > zCtx, `and above context menus (${zModal} > ${zCtx})`);

  // Clicking Delete twice must not produce two dialogs.
  await window.eval('openSettings("labels")');
  await tick(80);
  const del = doc.querySelector("[data-ldel]");
  del.click(); del.click(); del.click();
  await tick(60);
  ok(doc.querySelectorAll(".modal-back").length === 1,
     `repeated clicks open one dialog, not a stack (${doc.querySelectorAll(".modal-back").length})`);
  ok(doc.activeElement?.closest(".modal"), "and focus moves into it, so Enter cannot re-click the button behind");
  doc.querySelector(".modal [data-cancel]").click();
  await tick(40);
  ok(!doc.querySelector(".modal-back"), "cancel closes it");

  // Same for the filter editor and label editor.
  await window.eval('openSettings("filters")');
  await tick(80);
  const fe = doc.querySelector("[data-fedit]");
  fe.click(); fe.click();
  await tick(150);
  ok(doc.querySelectorAll(".modal-back").length === 1, "the filter editor does not stack either");
  doc.querySelector(".modal [data-cancel]").click();
  await tick(40);
  closeSheetIfOpen();

  // Label names reach dialog titles; a quote must not break the markup.
  window.eval(`ask({ title: 'Delete the label "A <b>bold</b> one"?', placeholder: null })`);
  await tick(20);
  const t = doc.querySelector(".modal-title");
  ok(t && t.textContent.includes('"A <b>bold</b> one"') && !t.querySelector("b"),
     "dialog titles are escaped");
  doc.querySelector(".modal [data-cancel]")?.click();
  await tick(20);
}

console.log("\nlabels in the sidebar");
{
  const lab = () => doc.querySelector('#catlist .node[data-scope="label:1"]');
  ok(!!lab(), "a label is listed under Categories");

  lab().dispatchEvent(new window.MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 50, clientY: 50 }));
  await tick(40);
  const items = [...doc.querySelectorAll(".ctx button")].map((b) => b.textContent);
  ok(items.some((t) => t.includes("Edit label")), `right-click offers Edit (${items.join("|")})`);
  ok(items.some((t) => t.includes("Delete label")), "and Delete");

  let n = calls.length;
  [...doc.querySelectorAll(".ctx button")].find((b) => b.textContent.includes("Delete label")).click();
  await tick(60);
  ok(doc.querySelector(".modal-title")?.textContent.includes("Important"),
     "delete asks first, naming the label");
  doc.querySelector(".modal [data-ok]").click();
  await tick(80);
  ok(calls.slice(n).some(([c, a]) => c === "delete_label" && a.id === 1), "and then deletes it");

  n = calls.length;
  lab().dispatchEvent(new window.MouseEvent("dblclick", { bubbles: true, cancelable: true }));
  await tick(80);
  const name = doc.querySelector(".modal [data-name]");
  ok(name && name.value === "Important", "double-clicking a label opens its editor");
  name.value = "Urgent";
  doc.querySelector(".modal [data-ok]").click();
  await tick(80);
  const saved4 = calls.slice(n).find(([c]) => c === "save_label");
  ok(saved4 && saved4[1].draft.id === 1 && saved4[1].draft.name === "Urgent",
     "and saving renames that label");
}

console.log("\nversion, exit");
{
  await window.eval('openSettings("about")');
  await tick(80);
  ok(doc.querySelector("[data-version]")?.textContent.includes("1.0.0"),
     `About shows the version from the app (${doc.querySelector("[data-version]")?.textContent})`);
  ok(doc.querySelector(".applogo")?.getAttribute("src") === "logo.png", "and the real app icon");
  closeSheetIfOpen();

  let n = calls.length;
  doc.dispatchEvent(new window.KeyboardEvent("keydown", { key: "q", ctrlKey: true, bubbles: true }));
  await tick(20);
  ok(calls.slice(n).some(([c]) => c === "quit_app"), "Ctrl+Q quits");
  const exit = window.eval("appMenu()").find((i) => i.label === "Exit");
  n = calls.length;
  exit.run();
  await tick(20);
  ok(calls.slice(n).some(([c]) => c === "quit_app"),
     "Exit quits rather than closing the window, which close-to-tray would turn into a hide");
}

console.log("\nbugs found in review");
{
  const item = (id) => doc.querySelector(`#list .item[data-id="${id}"]`);
  const selIds = () => [...doc.querySelectorAll('#list .item[data-sel="true"]')].map((n) => Number(n.dataset.id));
  closeSheetIfOpen();
  await window.eval('selectScope("all", "All")');
  await tick(40);

  // Search, then Ctrl+A and j: only what the search left visible.
  $("#q").value = "Fourth";
  $("#q").dispatchEvent(new window.Event("input"));
  await tick(320);
  doc.dispatchEvent(new window.KeyboardEvent("keydown", { key: "a", ctrlKey: true, bubbles: true }));
  await tick();
  let n0 = calls.length;
  $("#btn-delete").click();
  await tick(40);
  const d0 = calls.slice(n0).find(([c]) => c === "set_deleted");
  ok(d0 && JSON.stringify(d0[1].ids) === "[13]",
     `Ctrl+A then Delete with a search acts only on the visible rows (${d0 && JSON.stringify(d0[1].ids)})`);
  $("#q").value = "ir";   // First, Third; not Second or Fourth
  $("#q").dispatchEvent(new window.Event("input"));
  await tick(320);
  item(10).click();
  await tick(40);
  let n = calls.length;
  doc.dispatchEvent(new window.KeyboardEvent("keydown", { key: "j", bubbles: true }));
  await tick(40);
  const opened1 = calls.slice(n).find(([c]) => c === "article")?.[1].id;
  ok(opened1 === 12, `j skips articles the search hides (opened ${opened1})`);
  $("#q").value = "";
  $("#q").dispatchEvent(new window.Event("input"));
  await tick(320);

  // Two quick clicks: the slow first reply must not replace the second.
  delay.article[12] = 120;
  item(12).click();
  await tick(5);
  item(13).click();
  await tick(250);
  delay.article[12] = 0;
  ok($("#article .headline")?.textContent === "Article 13",
     `a slow reply for the previous article does not replace the current one (${$("#article .headline")?.textContent})`);

  // Same for the list: switching feeds fast shows the last one clicked.
  delay.news_list["feed:3"] = 120;
  window.eval('selectScope("feed:3", "Cold Storage")');
  await tick(5);
  window.eval('selectScope("feed:2", "Kernel Notes")');
  await tick(250);
  delay.news_list["feed:3"] = 0;
  ok(!$("#list").textContent.includes("feed:3"),
     "a slow reply for the previous feed does not replace the current list");

  // Dates: yesterday is Yesterday, not Today.
  const y = new Date(); y.setDate(y.getDate() - 1); y.setHours(23, 30, 0, 0);
  const y2 = new Date(); y2.setDate(y2.getDate() - 1); y2.setHours(0, 30, 0, 0);
  ok(window.eval(`dayKey("${y.toISOString()}")`) === "Yesterday"
     && window.eval(`dayKey("${y2.toISOString()}")`) === "Yesterday"
     && window.eval(`dayKey("${new Date().toISOString()}")`) === "Today",
     "yesterday's articles are grouped under Yesterday");

  // Enter on a focused button activates that button only.
  item(10).click();
  await tick(40);
  const before = opened.length;
  $("#btn-update").dispatchEvent(new window.KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
  doc.querySelector("#tree .node")?.dispatchEvent(new window.KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
  await tick(40);
  ok(opened.length === before, "Enter on a toolbar button or a tree row does not also open the browser");
  await window.eval('selectScope("all", "All")');
  await tick(40);
  item(10).click();
  await tick(40);
  doc.body.dispatchEvent(new window.KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
  await tick();
  ok(opened.length === before + 1, "Enter in the list still opens the article");

  // The reading pane's Delete acts on the article on screen.
  item(10).click();
  await tick(40);
  item(13).dispatchEvent(new window.MouseEvent("click", { ctrlKey: true, bubbles: true }));
  item(10).dispatchEvent(new window.MouseEvent("click", { ctrlKey: true, bubbles: true }));
  await tick();
  n = calls.length;
  $("#btn-readdelete").click();
  await tick(40);
  const del = calls.slice(n).find(([c]) => c === "set_deleted");
  ok(del && JSON.stringify(del[1].ids) === "[10]",
     `the reading pane Delete deletes the shown article (${del && JSON.stringify(del[1].ids)})`);

  // Esc in a dialog opened from Settings closes the dialog, not Settings.
  await window.eval('openSettings("labels")');
  await tick(60);
  doc.querySelector(".sheet [data-ledit], .sheet [data-lnew], .sheet [data-ldel]")?.click();
  await tick(40);
  const hadDialog = !!doc.querySelector(".modal-back");
  doc.body.dispatchEvent(new window.KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  await tick(40);
  ok(hadDialog && !doc.querySelector(".modal-back") && !!doc.querySelector(".sheet"),
     `Esc closes the dialog and leaves Settings open (dialog ${hadDialog}, still open ${!!doc.querySelector(".modal-back")}, sheet ${!!doc.querySelector(".sheet")})`);
  doc.body.dispatchEvent(new window.KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  await tick(20);
  ok(!doc.querySelector(".sheet"), "and a second Esc closes Settings");

  // "Mark as read when opened" off: opening does not mark it read.
  window.eval('applyTrayBehaviour({ "reading.mark_read_on_open": "0" })');
  await window.eval('selectScope("all", "All")');
  await tick(40);
  n = calls.length;
  item(12).click();
  await tick(60);
  ok(!calls.slice(n).some(([c]) => c === "set_read"), "with mark-read-on-open off, opening leaves it unread");
  window.eval('applyTrayBehaviour({ "reading.mark_read_on_open": "1" })');

  // A failed full-article fetch stops saying one is coming.
  item(10).click();
  await tick(40);
  $("#article .kicker").insertAdjacentHTML("beforeend", '<span class="chip pendingchip">fetching full article…</span>');
  await fire("article-ready", { id: 10, ok: false });
  await tick();
  ok(!doc.querySelector(".pendingchip"), "a failed full-article fetch removes 'fetching full article…'");
}

console.log("\nempty deleted from the folder");
{
  closeSheetIfOpen();
  const trash = doc.querySelector('#tree .node[data-scope="deleted"], .node[data-scope="deleted"]');
  trash.dispatchEvent(new window.MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 40, clientY: 40 }));
  await tick(40);
  const entry = [...doc.querySelectorAll(".ctx button")].find((b) => b.textContent.includes("Empty Deleted"));
  ok(!!entry, "right-clicking Deleted offers Empty Deleted…");
  entry?.click();
  await tick(40);
  const confirm = doc.querySelector(".modal-back [data-ok]");
  ok(!!confirm, "and asks before deleting for good");
  const n = calls.length;
  confirm?.click();
  await tick(60);
  ok(calls.slice(n).some(([c]) => c === "purge_deleted"), "confirming empties it");
}

console.log("\nsecond review");
{
  const tree = () => doc.querySelector("#tree");
  closeSheetIfOpen();
  await window.eval('selectScope("all", "All")');
  await tick(40);

  // Two opens in flight draw one sheet, and it closes.
  delay.get_settings = 60;
  window.eval('openSettings()');
  window.eval('openSettings()');
  await tick(200);
  delay.get_settings = 0;
  ok(doc.querySelectorAll(".sheet").length === 1, `Ctrl+, twice opens one Settings (${doc.querySelectorAll(".sheet").length})`);
  closeSheetIfOpen();
  await tick(20);
  ok(doc.querySelectorAll(".sheet").length === 0, "and it closes");

  // Feed properties offers seconds, so an imported interval survives a save.
  ivType.value = "seconds";
  await window.eval('openFeedSettings(2)');
  await tick(60);
  ok(doc.querySelector("#f-ivt")?.value === "seconds", "an interval in seconds shows as seconds");
  ivType.value = "minutes";
  // Esc closes Feed properties.
  doc.body.dispatchEvent(new window.KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  await tick(20);
  ok(!doc.querySelector(".sheet"), "Esc closes Feed properties");

  // Delete with keyboard focus on a tree row removes that row, not the selected one.
  const nodes = [...tree().querySelectorAll(".node[data-id]")];
  const feedA = nodes.find((n) => n.dataset.label === "Kernel Notes");
  const feedB = nodes.find((n) => n.dataset.label === "Cold Storage");
  feedA.click();
  await tick(40);
  feedB.focus();
  feedB.dispatchEvent(new window.KeyboardEvent("keydown", { key: "Delete", bubbles: true }));
  await tick(20);
  const title = doc.querySelector(".modal-back .modal-title")?.textContent || "";
  ok(title.includes("Cold Storage"), `Delete over the tree names the focused feed (${title})`);
  doc.querySelector(".modal-back [data-cancel]")?.click();
  await tick(20);

  // Double-clicking a folder's chevron toggles it and does not open Rename.
  const folder = tree().querySelector('.node[data-folder="true"]');
  folder.querySelector("[data-toggle]")?.dispatchEvent(new window.MouseEvent("dblclick", { bubbles: true }));
  await tick(20);
  ok(!doc.querySelector(".modal-back"), "double-clicking a folder's arrow does not open Rename");

  // Context menus close on scroll.
  await window.eval('selectScope("all", "All")');
  await tick(40);
  doc.querySelector("#list .item").dispatchEvent(new window.MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 30, clientY: 30 }));
  await tick(20);
  const had = !!doc.querySelector(".ctx");
  doc.querySelector("#list").dispatchEvent(new window.Event("scroll"));
  await tick(10);
  ok(had && !doc.querySelector(".ctx"), "a context menu closes when the page scrolls");

  // Compact density redraws label chips as squares.
  window.eval('setDensity("compact")');
  await tick(20);
  ok(!!doc.querySelector("#list .lchip.dotonly"), "switching to compact redraws label chips");
  const clonesPressed = [...doc.querySelectorAll('button[data-density="compact"]')].every((b) => b.getAttribute("aria-pressed") === "true");
  ok(clonesPressed, "every density button, clones included, shows the current density");
  window.eval('setDensity("relaxed")');
  await tick(20);

  // "Mark above as read" leaves out rows the search hides.
  $("#q").value = "Fourth";
  $("#q").dispatchEvent(new window.Event("input"));
  await tick(320);
  window.eval('applyTrayBehaviour({ "reading.mark_read_on_open": "0" })');
  doc.querySelector('#list .item[data-id="13"]').click();
  await tick(40);
  doc.querySelector('#list .item[data-id="13"]').dispatchEvent(new window.MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 30, clientY: 30 }));
  await tick(40);
  let n = calls.length;
  [...doc.querySelectorAll(".ctx button")].find((b) => b.textContent.includes("Mark above as read"))?.click();
  await tick(40);
  const marked = calls.slice(n).find(([c]) => c === "set_read");
  ok(marked && JSON.stringify(marked[1].ids) === "[13]", `mark above as read respects the search (${marked && JSON.stringify(marked[1].ids)})`);
  window.eval('applyTrayBehaviour({ "reading.mark_read_on_open": "1" })');
  $("#q").value = "";
  $("#q").dispatchEvent(new window.Event("input"));
  await tick(320);

  // Renaming the feed being shown renames the list header too.
  const kn = [...tree().querySelectorAll(".node[data-id]")].find((x) => x.dataset.label === "Kernel Notes");
  kn.click();
  await tick(40);
  await window.eval('renamed(2, "Kernel Notes 2")');
  ok($("#scopename").textContent === "Kernel Notes 2", "renaming the shown feed updates the header");

  // A label's own text colour survives an edit.
  const p = window.eval('editLabel({ id: 9, name: "Old", color_bg: "#123456", color_text: "#000000" })');
  await tick(20);
  doc.querySelector(".modal-back [data-ok]")?.click();
  const d = await p;
  ok(d && d.colorText === "#000000", `editing keeps the label's text colour (${d && d.colorText})`);

  // A filter naming several feeds is not shown, or saved, as "every feed".
  const fp = window.eval('editFilter({ id: 5, name: "multi", mode: 1, enabled: true, feeds: [2, 3], conditions: [{ field: "title", op: "contains", content: "x" }], actions: [{ action: "mark_read", params: null }] }, [])');
  await tick(60);
  const sel = [...doc.querySelectorAll(".modal-back select")].find((x) => [...x.options].some((o) => o.textContent.includes("every feed")));
  ok(sel && sel.value === "keep", `a multi-feed filter shows as its feeds, not every feed (${sel && sel.value})`);
  doc.querySelector(".modal-back").click();
  await fp;

  // Sound and notification actions.
  {
    picked.value = "C:\\Sounds\\ding.wav";
    const fp2 = window.eval('editFilter({ id: 6, name: "loud", mode: 1, enabled: true, feeds: null, conditions: [{ field: "title", op: "contains", content: "x" }], actions: [{ action: "mark_read", params: null }] }, [])');
    await tick(60);
    const box = doc.querySelector(".modal-back");
    const af = box.querySelector('[data-af="0"]');
    ok([...af.options].some((o) => o.textContent === "Play a sound")
       && [...af.options].some((o) => o.textContent === "Show a notification"),
       "the filter editor offers a sound and a notification");
    af.value = "play_sound"; af.dispatchEvent(new window.Event("change")); await tick(20);
    // Saving without a file refuses.
    box.querySelector("[data-ok]").click(); await tick(20);
    ok(!!doc.querySelector(".modal-back") && $("#toast").textContent.includes("sound file"),
       "a sound action needs a file");
    box.querySelector("[data-abrowse]") || null;
    doc.querySelector(".modal-back [data-abrowse]").click(); await tick(40);
    ok(doc.querySelector('.modal-back [data-av="0"]').value === "C:\\Sounds\\ding.wav", "Browse fills in the chosen file");
    let n = calls.length;
    doc.querySelector(".modal-back [data-aplay]").click(); await tick(20);
    ok(calls.slice(n).some(([c, a]) => c === "test_sound" && a.path === "C:\\Sounds\\ding.wav"), "the play button plays it");
    doc.querySelector(".modal-back [data-aadd]").click(); await tick(20);
    const af2 = doc.querySelector('.modal-back [data-af="1"]');
    af2.value = "notify"; af2.dispatchEvent(new window.Event("change")); await tick(20);
    doc.querySelector(".modal-back [data-ok]").click();
    const d2 = await fp2;
    ok(d2 && JSON.stringify(d2.actions) === JSON.stringify([
      { action: "play_sound", params: "C:\\Sounds\\ding.wav" }, { action: "notify", params: null }]),
      `both actions are saved (${d2 && JSON.stringify(d2.actions)})`);
    picked.value = null;

    // A sound the app hands to the page is read and played.
    n = calls.length;
    await fire("play-sound", "/home/me/ding.ogg");
    await tick(30);
    ok(calls.slice(n).some(([c, a]) => c === "read_sound" && a.path === "/home/me/ding.ogg"), "a sound event reads the file to play it");
  }

  // Newspaper: a failed load says so on the card.
  window.eval('setLayout("newspaper")');
  await tick(40);
  fail.article.add(12);
  doc.querySelector('#list .item[data-id="12"]').click();
  await tick(60);
  fail.article.delete(12);
  ok(doc.querySelector('#list .item[data-id="12"]')?.textContent.includes("Could not load"),
     "in newspaper a failed article says so on its card");
  window.eval('setLayout("classic")');
  await tick(40);

  // The Update button stays busy until the update call returns.
  delay.update_all = 120;
  $("#btn-update").click();
  await tick(10);
  await fire("update-progress", { done: 2, total: 2, title: "x", ok: true });
  await tick(10);
  const busy = $("#btn-update").disabled;
  await tick(200);
  delay.update_all = 0;
  ok(busy && !$("#btn-update").disabled, "Update stays busy until the update has been written, then frees");

  // Clean up now saves only the cleanup rules, and asks first.
  await window.eval('openSettings("cleanup")');
  await tick(80);
  n = calls.length;
  doc.querySelector('.sheet [data-act="cleanup"]')?.click();
  await tick(60);
  const saves = calls.slice(n).filter(([c]) => c === "set_settings");
  ok(saves.length === 0, "with no rule changes, Clean up now saves nothing");
  closeSheetIfOpen();
}

console.log("\nupdates");
{
  closeSheetIfOpen();
  // The Updates page saves the repository, then checks.
  await window.eval('openSettings("updates")');
  await tick(80);
  const repo = doc.querySelector('.sheet [data-num="updates.repo"]');
  ok(!!repo, "Settings has an Updates page with the repository");
  ok(repo.placeholder === "masterrite/SnapRSS", "an empty repository shows the default it falls back to");
  repo.value = "me/snaprss";
  repo.dispatchEvent(new window.Event("input"));
  upd.result = { version: "1.0.1", current: "1.0.0", notes: null };
  let n = calls.length;
  doc.querySelector('.sheet [data-act="checkupdate"]').click();
  await tick(60);
  const saved = calls.slice(n).find(([c]) => c === "set_settings");
  ok(saved && saved[1].values["updates.repo"] === "me/snaprss", "Check now saves the repository first");
  ok(calls.slice(n).some(([c]) => c === "check_update"), "and checks");
  ok(doc.querySelector("[data-updresult]").textContent.includes("1.0.1"), "and says what it found");
  ok(!!doc.querySelector("#updatebar"), "a found update offers itself in a bar");
  closeSheetIfOpen();

  // Install from the bar, with progress.
  n = calls.length;
  doc.querySelector("#updatebar [data-install]").click();
  await fire("update-download", { got: 50, total: 100 });
  await tick(20);
  ok(calls.slice(n).some(([c]) => c === "install_update"), "Install and restart installs");
  ok(doc.querySelector("#updatebar [data-msg]").textContent.includes("50%"), "with download progress");
  doc.querySelector("#updatebar")?.remove();

  // A failed install says why and does not offer the same install again.
  window.eval('showUpdateBar({ version: "1.0.1" })');
  upd.installError = "The download did not match its signature, so it was not installed.";
  doc.querySelector("#updatebar [data-install]").click();
  await tick(20);
  upd.installError = null;
  ok(doc.querySelector("#updatebar").textContent.includes("signature")
     && !doc.querySelector("#updatebar [data-install]"),
     "a failed install says why and removes the install button");
  doc.querySelector("#updatebar")?.remove();

  // The daily check announces itself the same way; Later dismisses it.
  await fire("update-available", { version: "1.0.2", current: "1.0.0" });
  await tick(10);
  ok(doc.querySelector("#updatebar")?.textContent.includes("1.0.2"), "the background check shows the bar");
  doc.querySelector("#updatebar [data-later]").click();
  ok(!doc.querySelector("#updatebar"), "Later dismisses it");

  // Newest already, and not set up yet.
  upd.result = null;
  await window.eval("checkForUpdates()");
  await tick(20);
  ok($("#toast").textContent.includes("newest"), "no update says so");
  upd.error = '"me" in Settings → Updates is not a GitHub owner/name or a URL';
  await window.eval("checkForUpdates()");
  await tick(80);
  ok(doc.querySelector('.sheet [data-num="updates.repo"]'), "an unusable repository opens the Updates page");
  upd.error = null;
  closeSheetIfOpen();
}

console.log("\ntext size and shortcuts");
{
  closeSheetIfOpen();
  const key = (k, o = {}) => doc.dispatchEvent(new window.KeyboardEvent("keydown", { key: k, bubbles: true, cancelable: true, ...o }));
  const scale = () => doc.documentElement.style.getPropertyValue("--text-scale");
  window.localStorage.removeItem("textScale");
  await window.eval("textReset()");
  key("=", { ctrlKey: true }); await tick(10);
  ok(scale() === "1.1", `Ctrl + makes the article text larger (${scale()})`);
  key("+", { ctrlKey: true, shiftKey: true }); await tick(10);
  ok(scale() === "1.2", "so does Ctrl and the + key itself");
  key("-", { ctrlKey: true }); await tick(10);
  ok(scale() === "1.1", "Ctrl - makes it smaller");
  key("0", { ctrlKey: true }); await tick(10);
  ok(scale() === "1" && window.localStorage.getItem("textScale") === "1", "Ctrl 0 resets it, and it is remembered");
  $("#article").dispatchEvent(new window.WheelEvent("wheel", { deltaY: -100, ctrlKey: true, bubbles: true, cancelable: true }));
  ok(scale() === "1.1", "Ctrl + wheel over the article works too");
  await window.eval("textReset()");

  // Rebinding: Star moves from S to G.
  await window.eval('openSettings("shortcuts")'); await tick(80);
  const starBtn = doc.querySelector('[data-rebind="star"]');
  ok(starBtn?.textContent.trim() === "S", "the Shortcuts page shows the current key");
  starBtn.click(); await tick(10);
  ok(starBtn.textContent.includes("Press a key"), "clicking it waits for a key");
  key("Shift"); await tick(10);
  ok(!!doc.querySelector('[data-rebind="star"][aria-pressed="true"]'), "a modifier on its own is not a key");
  key("g"); await tick(60);
  ok(doc.querySelector('[data-rebind="star"]').textContent.trim() === "G", "the new key is shown");
  ok(!!doc.querySelector(".sheet"), "and pressing it did not close Settings or act on the list");
  ok(JSON.parse(window.localStorage.getItem("keymap")).star === "G", "and stored, only the change");
  // Taking a key another action has moves it.
  doc.querySelector('[data-rebind="toggleRead"]').click(); await tick(10);
  key("g"); await tick(60);
  ok(doc.querySelector('[data-rebind="star"]').textContent.includes("none"), "a key taken by another action leaves the first");
  ok($("#toast").textContent.includes("moved from"), "and says so");
  // Esc while waiting cancels without closing Settings.
  doc.querySelector('[data-rebind="star"]').click(); await tick(10);
  key("Escape"); await tick(60);
  ok(!!doc.querySelector(".sheet") && doc.querySelector('[data-rebind="star"]').textContent.includes("none"),
     "Esc cancels the change and leaves Settings open");
  doc.querySelector('[data-rebind="star"]').click(); await tick(10);
  key("x"); await tick(60);
  closeSheetIfOpen();
  ok(($('#readbar [data-cmd="star"], #btn-star2')?.title || "").includes("(X)"), "tooltips follow the new key");

  // The new key works, the old one no longer does.
  click('#tree .node[data-id="2"]'); await tick(80);
  click('.item[data-id="10"]'); await tick(80);
  let n = calls.length;
  key("s"); await tick(40);
  ok(!calls.slice(n).some(([c]) => c === "set_starred"), "the old key does nothing");
  key("x"); await tick(40);
  ok(calls.slice(n).some(([c]) => c === "set_starred"), "the new key stars");
  key("x"); await tick(40);
  // Reset all puts the defaults back.
  await window.eval('openSettings("shortcuts")'); await tick(80);
  doc.querySelector("[data-keyresetall]").click(); await tick(60);
  ok(!window.localStorage.getItem("keymap") && doc.querySelector('[data-rebind="star"]').textContent.trim() === "S",
     "Reset all restores every default");
  closeSheetIfOpen();
  // A key recorded while Settings closes does not keep swallowing keys.
  await window.eval('openSettings("shortcuts")'); await tick(80);
  doc.querySelector('[data-rebind="star"]').click(); await tick(10);
  closeSheetIfOpen();
  n = calls.length;
  key("s"); await tick(40);
  ok(calls.slice(n).some(([c]) => c === "set_starred"), "closing Settings mid-change leaves the keyboard working");
  key("s"); await tick(40);
}

console.log("\nstartup window and About");
{
  const wr = calls.findIndex(([c]) => c === "window_ready");
  const ft = calls.findIndex(([c]) => c === "feed_tree");
  ok(wr > ft && ft >= 0, "the window is shown after the content has loaded, not before");
  await window.eval('openSettings("about")'); await tick(80);
  ok(!doc.querySelector(".sheet").textContent.includes("Imports QuiteRSS"), "About no longer says what it imports");
  closeSheetIfOpen();
}

console.log("\ntray count setting");
{
  await window.eval('openSettings("general")'); await tick(80);
  const t = doc.querySelector('[data-sw="tray.show_unread"]');
  ok(t && t.getAttribute("aria-checked") === "true", "the tray count is on by default and can be switched off");
  t.click();
  const n = calls.length;
  doc.querySelector(".sheet [data-save]").click(); await tick(80);
  const saved = calls.slice(n).find(([c]) => c === "set_settings");
  ok(saved && saved[1].values["tray.show_unread"] === "0", "switching it off is saved");
  ok(calls.slice(n).some(([c]) => c === "counts"), "and the tray is brought up to date straight away");
}

console.log("\nsite icons");
{
  ok($('#tree .node[data-id="2"] img.favicon')?.getAttribute("src").startsWith("data:image/png"),
     "a feed with an icon shows it in the tree");
  ok(!!$('#tree .node[data-id="3"] span.favicon'), "a feed without one keeps its letter");
  click('#tree .node[data-id="2"]'); await tick(80);
  ok(!!$('#list .item[data-id="10"] img.favicon.tiny'), "and on its articles in the list");
  icons.value = { 2: icons.value[2], 3: "data:image/png;base64,iVBORw0KGgoAAAANSUhEUg==" };
  await fire("icons-updated", null); await tick(60);
  ok(!!$('#tree .node[data-id="3"] img.favicon'), "a newly found icon appears without a restart");
  icons.value = { 2: icons.value[2] };
  await fire("icons-updated", null); await tick(60);
  ok(calls.some(([c, a]) => c === "refresh_feed_icon" && a.id === 42), "adding a feed looks up its icon");
}

console.log("\nolder articles, search and sorting");
{
  closeSheetIfOpen();
  $("#q").value = ""; $("#q").dispatchEvent(new window.Event("input")); await tick(320);
  await window.eval('selectScope("label:77", "Many")');
  await tick(60);
  ok(doc.querySelectorAll("#list .item").length === 500, "a long list loads one page first");
  ok(!!doc.querySelector("#list .more"), "and says there is more");
  let n = calls.length;
  await window.eval("loadMore()");
  await tick(40);
  const page2 = calls.slice(n).find(([c]) => c === "news_list");
  ok(page2 && page2[1].offset > 0, "scrolling on asks for the next page");
  ok(doc.querySelectorAll("#list .item").length === 1000, "and adds it without duplicates");
  await window.eval("loadMore()"); await tick(40);
  ok(doc.querySelectorAll("#list .item").length === 1234 && !doc.querySelector("#list .more"),
     "every article is reachable, and the end says so by stopping");
  const ids = [...doc.querySelectorAll("#list .item")].map((e) => e.dataset.id);
  ok(new Set(ids).size === ids.length, "no article appears twice");

  // In Unread, articles read since loading have left the scope on the
  // server: the next page starts after the rows still unread.
  bigUnread.value = true;
  await window.eval('selectScope("unread", "Unread")'); await tick(60);
  const loaded = doc.querySelectorAll("#list .item").length;
  for (const d of [...doc.querySelectorAll("#list [data-dot]")].slice(0, 3)) { d.click(); await tick(20); }
  n = calls.length;
  await window.eval("loadMore()"); await tick(40);
  const up = calls.slice(n).find(([c]) => c === "news_list");
  ok(loaded === 500 && up && up[1].offset === 500 - 3 - 50,
     `in Unread the next page counts only rows still unread (${up && up[1].offset})`);
  bigUnread.value = false;
  await window.eval('selectScope("label:77", "Many")'); await tick(60);
  await window.eval("loadMore()"); await tick(40);
  await window.eval("loadMore()"); await tick(40);

  // A reload of the same list keeps what was loaded.
  n = calls.length;
  await window.eval("loadList()"); await tick(40);
  const re = calls.slice(n).find(([c]) => c === "news_list");
  ok(re && re[1].limit >= 1234 && doc.querySelectorAll("#list .item").length === 1234,
     "a refresh keeps every loaded row instead of dropping back to one page");

  // Search goes to the database, over the whole scope.
  n = calls.length;
  $("#q").value = "Old post 1200"; $("#q").dispatchEvent(new window.Event("input"));
  await tick(320);
  const sq = calls.slice(n).find(([c]) => c === "news_list");
  ok(sq && sq[1].query === "Old post 1200" && sq[1].scope === "label:77", "search is sent to the database for the scope");
  ok(doc.querySelectorAll("#list .item").length === 1
     && doc.querySelector("#list .item .t").textContent === "Old post 1200",
     "and finds an article that was never loaded");
  $("#q").value = ""; $("#q").dispatchEvent(new window.Event("input")); await tick(320);

  // With nothing selected, a search looks everywhere.
  await window.eval("clearScope()"); await tick(40);
  ok(doc.querySelector("#list .empty")?.textContent.includes("Select a feed"), "no scope, no search: nothing listed");
  n = calls.length;
  $("#q").value = "Second"; $("#q").dispatchEvent(new window.Event("input")); await tick(320);
  const sa = calls.slice(n).find(([c]) => c === "news_list");
  ok(sa && sa[1].scope === "all", "no scope: the search covers every article");
  $("#q").value = ""; $("#q").dispatchEvent(new window.Event("input")); await tick(320);

  // Sorting from the column headings.
  click('#tree .node[data-id="2"]'); await tick(80);
  n = calls.length;
  click('#listhead [data-sort="title"]'); await tick(60);
  let so = calls.slice(n).find(([c]) => c === "news_list");
  ok(so && so[1].sort === "title", "clicking TITLE sorts A to Z");
  const titles = [...doc.querySelectorAll("#list .item .t")].map((e) => e.textContent);
  ok(JSON.stringify(titles) === JSON.stringify([...titles].sort((a, b) => a.localeCompare(b))), "in that order");
  ok(!doc.querySelector("#list .group"), "without day headings");
  ok($('#listhead [data-sort="title"]').textContent.includes("▲"), "the heading shows the direction");
  click('#listhead [data-sort="title"]'); await tick(60);
  ok(window.localStorage.getItem("sort") === "-title", "a second click reverses, and is remembered");
  click('#listhead [data-sort="date"]'); await tick(60);
  ok(window.localStorage.getItem("sort") === "-date" && !!doc.querySelector("#list .group"),
     "DATE goes back to newest first with day headings");
  await window.eval('setSort("feed")'); await tick(60);
  ok(JSON.stringify([...doc.querySelectorAll("#list .group")].map((g) => g.textContent))
     === JSON.stringify(["COLD STORAGE", "KERNEL NOTES"]), "sorting by feed heads each feed's articles, A to Z");
  await window.eval('setSort("-date")'); await tick(60);
}

console.log("\nthird review");
{
  closeSheetIfOpen();
  const press = (k, o = {}, target = doc.body) => {
    const e = new window.KeyboardEvent("keydown", { key: k, bubbles: true, cancelable: true, ...o });
    target.dispatchEvent(e);
    return e;
  };
  const pages = (from) => calls.slice(from).filter(([c]) => c === "news_list").map(([, a]) => a);
  const st = (expr) => window.__appEval(expr);
  const until = async (cond, ms = 8000) => { const t0 = Date.now(); while (!cond() && Date.now() - t0 < ms) await tick(20); };
  window.eval('applyTrayBehaviour({ "reading.mark_read_on_open": "0" })');

  // Enter on a focused Cancel is Cancel. The dialog took every Enter as OK,
  // so tabbing to Cancel and pressing Enter removed the feed.
  let n = calls.length;
  const removing = window.eval(`removeNode(document.querySelector('#tree .node[data-id="3"]'))`);
  await tick(10);
  const cancel = $(".modal-back [data-cancel]");
  cancel.focus();
  const ent = press("Enter", {}, cancel);
  await tick(20);
  ok(!calls.slice(n).some(([c]) => c === "remove_feed") && !ent.defaultPrevented,
     "Enter on a focused Cancel does not remove the feed");
  cancel.click();
  await removing;
  const asking = window.eval('ask({ title: "Name it", value: "typed" })');
  await tick(10);
  press("Enter", {}, $(".modal-input"));
  ok((await asking) === "typed", "Enter in a dialog's text field still means OK");
  const confirming = window.eval('ask({ title: "Sure?", placeholder: null })');
  await tick(10);
  press("Enter", {}, doc.activeElement);
  ok((await confirming) === true, "and so does Enter on its focused OK button");

  // Scrolling while another feed loads used the old list's length as the new
  // feed's offset: rows 0-499 then 950 onwards, with a gap between.
  await window.eval('selectScope("label:77", "Many")');
  delay.news_list["feed:2"] = 80;
  n = calls.length;
  const switching = window.eval('selectScope("feed:2", "Kernel Notes")');
  await window.eval("loadMore()");
  await switching;
  await tick(100);
  delete delay.news_list["feed:2"];
  ok(!pages(n).some((a) => a.offset > 0) && doc.querySelectorAll("#list .item").length === 4,
     `scrolling while a feed loads does not page it from the old list's length (${JSON.stringify(pages(n).map((a) => a.offset))})`);

  // The same during the search box's pause: the new search's rows were
  // appended to the old list.
  await window.eval('selectScope("label:77", "Many")');
  n = calls.length;
  $("#q").value = "Old post 12";
  $("#q").dispatchEvent(new window.Event("input"));
  await window.eval("loadMore()");
  ok(!pages(n).some((a) => a.offset > 0), "scrolling during the search pause does not add the search's rows to the old list");
  await tick(320);
  const found = [...doc.querySelectorAll("#list .item .t")].map((t) => t.textContent);
  ok(found.length > 0 && found.every((t) => t.includes("12")),
     `and the search then shows only its own rows (${found.length}: ${found.slice(0, 3)})`);
  $("#q").value = "";
  $("#q").dispatchEvent(new window.Event("input"));
  await tick(320);

  // A reply that lands after the scope changed is dropped.
  delay.news_list["label:77@450"] = 80;
  const paging = window.eval("loadMore()");
  await tick(10);
  await window.eval('selectScope("feed:2", "Kernel Notes")');
  await paging;
  delete delay.news_list["label:77@450"];
  ok(doc.querySelectorAll("#list .item").length === 4, "a page that arrives after switching feeds is not appended");

  // At 20000 rows the list stopped paging but kept saying "Loading more…".
  // renderList draws every row and takes ~12 s for 20000 in jsdom, so it is
  // switched off while the list is that long.
  bigRows.value = 20600;
  await window.eval('selectScope("label:77", "Many")');
  st(`window.realRenderList = renderList; renderList = () => {};
    state.items = Array.from({ length: 20000 }, (_, k) => ({ ...state.items[0], id: 5000 + k }));
    state.more = true;`);
  n = calls.length;
  await window.eval("loadMore()");
  const far = pages(n)[0];
  ok(far && far.offset === 19950 && st("state.items.length") === 20500,
     `past 20000 rows scrolling still loads older articles (${far && far.offset}, ${st("state.items.length")})`);
  st("renderList = window.realRenderList");
  bigRows.value = 1234;
  await window.eval('selectScope("feed:2", "Kernel Notes")');

  // J on the last loaded row loads the next page and goes on; the row is
  // scrolled into view in the classic layout too.
  const realScroll = window.Element.prototype.scrollIntoView;
  const scrolled = [];
  window.Element.prototype.scrollIntoView = function (o) { scrolled.push([this.dataset?.id, o?.block]); };
  await window.eval('selectScope("label:77", "Many")');
  await st("openArticle(state.items.at(-1).id)");
  n = calls.length;
  scrolled.length = 0;
  press("j");
  await until(() => st("state.selected") === 5500);
  ok(pages(n).some((a) => a.offset > 0) && st("state.selected") === 5500,
     `J on the last loaded row loads more and moves on (${st("state.selected")})`);
  ok(doc.documentElement.dataset.layout !== "newspaper" && scrolled.some(([id, b]) => id === "5500" && b === "nearest"),
     "and scrolls the row into view in the classic layout");
  press("k");
  await tick(20);
  ok(scrolled.some(([id]) => id === "5499"), "K does too");
  if (realScroll) window.Element.prototype.scrollIntoView = realScroll;
  else delete window.Element.prototype.scrollIntoView;

  // A new feed opens at its top.
  $("#list").scrollTop = 4000;
  await window.eval('selectScope("feed:2", "Kernel Notes")');
  ok($("#list").scrollTop === 0, `a new feed opens scrolled to the top (${$("#list").scrollTop})`);

  // Clicking a feed focuses its row. J then read an article, but Delete
  // still went to the focused feed and asked to remove it.
  const kn = doc.querySelector('#tree .node[data-id="2"]');
  kn.focus();
  kn.click();
  await tick(60);
  press("j", {}, kn);
  await tick(40);
  n = calls.length;
  press("Delete", {}, doc.activeElement);
  await tick(40);
  ok(!$(".modal-back") && calls.slice(n).some(([c]) => c === "set_deleted"),
     "after J from a clicked feed, Delete deletes the article, not the feed");
  closeSheetIfOpen();
  // A key the focused row handled itself is not also a shortcut.
  window.eval('{ const m = keymap(); m.next = "Space"; saveKeymap(m); }');
  await window.eval("openArticle(10)");
  kn.focus();
  n = calls.length;
  press(" ", {}, kn);
  await tick(60);
  ok(pages(n).length === 1 && !calls.slice(n).some(([c]) => c === "article"),
     "Space on a focused feed opens the feed and does not also go to the next article");
  window.localStorage.removeItem("keymap");
  window.eval("refreshKeyTips()");

  // The reading pane's star kept its label and followed a changed shortcut.
  window.eval('{ const m = keymap(); m.star = "F"; saveKeymap(m); }');
  await window.eval("openArticle(10)");
  const s2 = $("#btn-star2");
  ok(!!s2.querySelector(".tlabel") && !!s2.querySelector("svg") && s2.title === "Star (F)",
     `the reading pane star keeps its label and shortcut after an article opens (${s2.title})`);
  await st("toggleStar({ onlyShown: true })");
  window.eval("refreshKeyTips()");
  ok(!!s2.querySelector(".tlabel") && s2.title === "Unstar (F)", `and once starred (${s2.title})`);
  await st("toggleStar({ onlyShown: true })");
  window.localStorage.removeItem("keymap");
  window.eval("refreshKeyTips()");

  // Shift+J and Shift+K with nothing open start at the end they head from.
  st("state.selected = null; state.sel = new Set(); state.anchor = null; renderList()");
  press("J", { shiftKey: true });
  await tick(30);
  ok(st("state.selected") === st("state.items[0].id"), "Shift+J with nothing open selects the first row");
  st("state.selected = null; state.sel = new Set(); state.anchor = null; renderList()");
  press("K", { shiftKey: true });
  await tick(30);
  ok(st("state.selected") === st("state.items.at(-1).id"), "Shift+K with nothing open selects the last row");

  // A key given to another action wins over a built-in second key.
  window.eval('{ const m = keymap(); m.textReset = "Ctrl++"; saveKeymap(m); }');
  window.eval("setTextScale(1.5, { quiet: true })");
  press("+", { ctrlKey: true, shiftKey: true });
  ok(st("textScale()") === 1, `Ctrl++ bound to Normal text size resets it (${st("textScale()")})`);
  window.localStorage.removeItem("keymap");
  press("+", { ctrlKey: true, shiftKey: true });
  ok(st("textScale()") > 1, "unbound, Ctrl++ still makes text larger");
  window.eval("setTextScale(1, { quiet: true })");
  window.eval("refreshKeyTips()");

  // Esc in a Feed properties field closes it; in the search box it blurs.
  await window.eval("openFeedSettings(2)");
  await tick(40);
  $("#f-name").focus();
  press("Escape", {}, $("#f-name"));
  await tick(20);
  ok(!$(".sheet-back"), "Esc in a Feed properties field closes the sheet");
  $("#q").focus();
  press("Escape", {}, $("#q"));
  ok(doc.activeElement !== $("#q"), "Esc in the search box still just leaves it");

  // The status bar counts follow an update instead of waiting 15 s.
  countsNow.value = { unread: 7, total: 12, starred: 1 };
  click("#btn-update");
  await tick(80);
  ok($("#st-counts").textContent === "7 unread · 12 articles", `Update all refreshes the status bar (${$("#st-counts").textContent})`);
  countsNow.value = { unread: 8, total: 13, starred: 1 };
  await fire("feeds-updated", 1);
  await tick(40);
  ok($("#st-counts").textContent === "8 unread · 13 articles", "and so does a background update");
  countsNow.value = { unread: 3, total: 9, starred: 1 };
  window.eval('applyTrayBehaviour({ "reading.mark_read_on_open": "1" })');
}

console.log("\nlist drawing");
{
  closeSheetIfOpen();
  const press = (k, o = {}, target = doc.body) => {
    const e = new window.KeyboardEvent("keydown", { key: k, bubbles: true, cancelable: true, ...o });
    target.dispatchEvent(e);
    return e;
  };
  const st = (expr) => window.__appEval(expr);
  const row = (id) => doc.querySelector(`#list .item[data-id="${id}"]`);
  window.eval('applyTrayBehaviour({ "reading.mark_read_on_open": "0" })');
  bigRows.value = 1234;
  await window.eval('selectScope("label:77", "Many")');
  await st("openArticle(5003)");
  await tick(20);

  // J redraws the two rows whose state changed and leaves every other row
  // as it was. The whole list was rebuilt on each key.
  const far = row(5010), first = row(5000);
  press("j");
  await tick(40);
  ok(st("state.selected") === 5004 && row(5004).getAttribute("aria-selected") === "true"
     && row(5003).getAttribute("aria-selected") === "false" && row(5004).dataset.sel === "true"
     && row(5003).dataset.sel === "false",
     "J moves the highlight and the selection to the next row");
  ok(row(5010) === far && row(5000) === first, "and leaves the other rows alone");

  // Starring from the row redraws that row only.
  row(5010).querySelector("[data-star]").click();
  await tick(40);
  ok(row(5010) !== far && row(5010).querySelector("[data-star]").outerHTML !== far.querySelector("[data-star]").outerHTML
     && row(5000) === first, "the star on a row redraws just that row");
  row(5010).querySelector("[data-star]").click();
  await tick(40);

  // A click on a redrawn row still works: the list listens once, not per row.
  row(5010).click();
  await tick(40);
  ok(st("state.selected") === 5010, "a redrawn row still opens on click");
  row(5012).dispatchEvent(new window.MouseEvent("click", { bubbles: true, shiftKey: true }));
  await tick(40);
  ok(JSON.stringify([...doc.querySelectorAll('#list .item[data-sel="true"]')].map((n) => n.dataset.id))
     === JSON.stringify(["5010", "5011", "5012"]), "and shift-click still selects the range");

  // A new page is added below; the rows above are kept, and a heading that
  // runs across the join is not drawn twice.
  const loaded = st("state.items.length");
  await st("loadMore()");
  await tick(20);
  const groups = [...doc.querySelectorAll("#list .group")].map((g) => g.textContent);
  ok(st("state.items.length") > loaded && row(5000) === first
     && doc.querySelectorAll("#list .item").length === st("state.items.length"),
     `a page is added below without redrawing the rows above (${loaded} → ${st("state.items.length")})`);
  ok(groups.every((g, i) => i === 0 || g !== groups[i - 1]), `and no heading repeats at the join (${groups})`);
  ok(doc.querySelectorAll("#list .more").length === 1 && $("#list").lastElementChild.classList.contains("more"),
     "with one “Loading more” row, at the end");
  while (st("state.more")) await st("loadMore()");
  ok(!$("#list .more") && doc.querySelectorAll("#list .item").length === 1234, "which goes once everything is loaded");
  // Articles that arrive below the loaded ones (older dates, or sorted
  // oldest first) can still be scrolled to after an update.
  bigRows.value = 1300;
  await fire("feeds-updated", 66);
  await tick(60);
  ok(!!$("#list .more") && st("state.more") && row(5000) === first, "an update that adds articles below brings back “Loading more”");
  while (st("state.more")) await st("loadMore()");
  ok(doc.querySelectorAll("#list .item").length === 1300, `and they can be scrolled to (${doc.querySelectorAll("#list .item").length})`);
  bigRows.value = 1234;

  // Deep in a long list a key touches the same few rows as at the top.
  bigRows.value = 5000;
  await window.eval('selectScope("label:77", "Many")');
  while (st("state.more")) await st("loadMore()");
  await st("openArticle(9000)");
  await tick(20);
  const top = row(5000), deep = row(9990);
  for (let k = 0; k < 5; k++) { press("j"); await tick(0); }
  await tick(40);
  ok(st("state.selected") === 9005 && row(5000) === top && row(9990) === deep
     && doc.querySelectorAll("#list .item").length === 5000,
     `with 5000 rows loaded J redraws only the rows it changes (${st("state.selected")}, ${row(5000) === top}, ${row(9990) === deep}, ${doc.querySelectorAll("#list .item").length}, more ${st("state.more")})`);

  // A background update that brings back the same articles redraws only the
  // ones that changed. Every update rebuilt all 5000 rows.
  countsNow.value = { unread: 3, total: 9, starred: 1 };
  await fire("feeds-updated", 0);
  await tick(60);
  ok(row(5000) === top && row(9990) === deep, "an update that changes nothing leaves the rows alone");

  // New site icons swap only the small icon in each row.
  const icon = row(9990).querySelector(".m .favicon");
  icons.value = { 2: "data:image/png;base64,iVBORw0KGgoAAAANSUhEUg==", 3: "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAB" };
  await fire("icons-updated");
  await tick(60);
  ok(row(9990) === deep && row(9990).querySelector(".m .favicon") === icon,
     "new icons for other feeds leave these rows as they were");
  icons.value = { 2: "data:image/png;base64,iVBORw0KGgoAAAANSUhEUg==" };
  bigRows.value = 1234;

  // Newspaper: the open card follows J, and the one left behind closes.
  window.eval('setLayout("newspaper")');
  await tick(60);
  await st("openArticle(5001)");
  await tick(40);
  ok(!!row(5001).querySelector(".full, .ex") && !!row(5001).querySelector(".cardbar"), "a newspaper card opens");
  press("j");
  await tick(60);
  ok(!row(5001).querySelector(".cardbar") && !!row(5002).querySelector(".cardbar"),
     "J moves the open card to the next article and closes the last");
  row(5002).querySelector("[data-collapse]").click();
  await tick(20);
  ok(!row(5002).querySelector(".cardbar") && st("state.selected") === null, "Collapse closes it");
  window.eval('setLayout("classic")');
  await tick(60);
  window.eval('applyTrayBehaviour({ "reading.mark_read_on_open": "1" })');
}

console.log("\nno uncaught errors");
ok(consoleErrors.length === 0, `console clean${consoleErrors.length ? ": " + consoleErrors.join(" | ") : ""}`);

console.log(`\n${failures === 0 ? "ALL PASS" : failures + " FAILED"}`);
process.exit(failures === 0 ? 0 : 1);
