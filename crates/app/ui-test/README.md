# ui-test

Runs `ui/index.html` and `ui/app.js` in jsdom with a stubbed Tauri bridge, then
clicks every toolbar button and checks the right command was invoked.

    cd crates/app/ui-test && npm install && npm test

Three rounds of frontend bugs shipped as "the window opens but nothing
responds", which is what any uncaught error on load looks like.

Three classes of bug are covered separately:

* **Runtime.** The harness evaluates `app.js` and drives the DOM, so a thrown
  exception, a missing element or a wrong `invoke` argument name fails a test.
* **Configuration.** The harness evaluates `app.js` by hand, so it cannot
  notice a CSP that blocks the script or a missing `window.__TAURI__`. Those
  are checked statically against `tauri.conf.json`, `index.html` and
  `capabilities/default.json` before the DOM tests run.

* **Things jsdom cannot enforce.** jsdom applies no CSP, so a colour written
  into a style attribute works here and is dropped by the real webview, where
  Tauri's nonce makes `'unsafe-inline'` inert. Every runtime colour in the app
  was affected and every test passed. A static check now fails if a colour is
  authored into a style attribute, and the live assertions read `el.style`
  after `paint()` rather than the markup.

The suite is kept honest by sabotage: change the behaviour a test describes,
check the test fails. Rebuilding toolbar buttons instead of moving them,
reading operator indices from one flat table, and making the drop zone ignore
the pointer position have each been tried; each is caught.

It does not exercise the Rust side, WebView2 quirks, or layout and appearance.
Passing here is not evidence that anything is visible.
